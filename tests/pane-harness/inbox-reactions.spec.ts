// SPDX-License-Identifier: MIT
import { inboxFixtureRow, type InboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import type { InboxMessageRow } from "../../src/tauri/commands";
import { expect, test, type MountedPane, type PaneFixtures } from "./harness";
import { builder, INBOX_PANE, outgoing, scrollBubbleTo } from "./inbox-message-view-fixtures";

const team = { org: "fixture", team: "dev" };
const rook = "/kit/v1/avatars/builtin-chess/rook.svg";
const reviewer = {
  id: "reviewer",
  sessionId: "reviewer",
  handle: "reviewer",
  displayName: "Reviewer",
  rowNames: { primary: "Reviewer", secondary: null, full: "Reviewer" },
  state: "running",
  alive: true,
  avatar: "builtin-chess:queen",
  team,
};
const row = (id: number, text: string, minutesAgo: number, overrides: Partial<InboxMessageRow>) =>
  inboxFixtureRow(
    id,
    text,
    {
      from: "builder",
      fromName: "Builder",
      conversation: "builder",
      contentType: "markdown",
      unread: false,
      createdAtMs: Date.now() - minutesAgo * 60_000,
      ...overrides,
    },
    team,
  );

function world(): PaneFixtures {
  return {
    width: 420,
    height: 680,
    theme: { id: "codesense", scheme: "light" },
    sessions: [{ ...builder, face: { kind: "image", src: rook } }, reviewer],
    topics: {
      agentDefinitions: [
        {
          id: "definition:scout",
          rowType: "definition",
          row: { key: "scout", name: "Scout", avatar: "builtin-funny:yaml-wizard" },
        },
      ],
    },
    inbox: [
      ...Array.from({ length: 20 }, (_, i) =>
        row(
          10 + i,
          `Earlier progress note ${i}: still working through the checklist.`,
          200 - i * 9,
          {
            ...(i % 3 === 2 ? outgoing : {}),
          },
        ),
      ),
      row(1, "Theme check is green.", 12, {
        reactions: [
          { emoji: "👍", count: 2, reactors: ["human", "reviewer"] },
          { emoji: "👀", count: 1, reactors: ["builder"] },
        ],
      }),
      row(2, "Ship it once the theme check is green.", 10, {
        ...outgoing,
        reactions: [{ emoji: "✅", count: 1, reactors: ["builder"] }],
      }),
      row(3, "Should I tag 0.0.26 now?", 8, {
        kind: "needs-you",
        reactions: [{ emoji: "✅", count: 1, reactors: ["human"] }],
      }),
      inboxFixtureRow(
        40,
        "Scouting report attached.",
        {
          from: "scout",
          fromName: "Scout",
          conversation: "scout",
          contentType: "markdown",
          unread: false,
          createdAtMs: Date.now() - 300 * 60_000,
        },
        team,
      ),
    ],
  };
}

async function openBuilder(
  mountPane: (p: string, f: PaneFixtures) => Promise<MountedPane>,
  fixtures = world(),
  viewportHeight = 800,
) {
  const pane = await mountPane(INBOX_PANE, fixtures);
  await pane.page.setViewportSize({ width: 1000, height: viewportHeight });
  await pane.frame.locator(".ib-row", { hasText: "Builder" }).click();
  await expect(msg(pane, 3)).toBeVisible();
  return pane;
}
/** Sixty messages with Builder, of which the rollup window holds the newest thirty. */
const historyRows = (overrides: Partial<InboxMessageRow>) =>
  Array.from({ length: 60 }, (_, i) =>
    row(i + 1, `History note ${i + 1}.`, 600 - i * 5, overrides),
  );
/** Opens Builder's thread and pages back until message 5, outside the window, is loaded. */
async function openOldHistory(
  mountPane: (p: string, f: PaneFixtures) => Promise<MountedPane>,
  inbox: PaneFixtures["inbox"],
) {
  const pane = await mountPane(INBOX_PANE, {
    width: 420,
    height: 680,
    theme: { id: "codesense", scheme: "light" },
    sessions: [builder],
    rollupWindow: 30,
    inbox,
  });
  await pane.page.setViewportSize({ width: 1000, height: 800 });
  await pane.frame.locator(".ib-row", { hasText: "Builder" }).click();
  await expect(msg(pane, 60)).toBeVisible();
  await pane.frame.locator("#log").evaluate((el) => (el.scrollTop = 0));
  await expect(msg(pane, 5)).toBeAttached();
  return pane;
}
/** Builder's 👀 lands on `old` in the engine, then the committed inbox change names
 *  `named` (none: a change that names no rows). */
async function builderReacts(pane: MountedPane, old: InboxFixtureRow, named: number[]) {
  const reaction = { emoji: "👀", count: 1, reactors: ["builder"] };
  await pane.page.evaluate(
    ([r, t, ids]) => {
      const w = window as any;
      w.upsert("inbox", r);
      w.inboxChanged(t, ids);
    },
    [
      {
        ...old,
        message: { ...old.message, reactions: [reaction] },
        conversation: { ...old.conversation, reactions: [reaction] },
      },
      team,
      named,
    ] as const,
  );
}
const msg = (pane: MountedPane, id: number) => pane.frame.locator(`[data-message-id="${id}"]`);
const rect = (pane: MountedPane, selector: string) =>
  pane.frame
    .locator(selector)
    .first()
    .evaluate((el) => el.getBoundingClientRect().toJSON());
async function openStrip(pane: MountedPane, id: number, offset = 220) {
  await (await scrollBubbleTo(pane, id, offset)).hover();
  const strip = msg(pane, id).locator(".ib-react");
  await expect(strip).toHaveAttribute("data-open", "true");
  return strip;
}
async function openPicker(pane: MountedPane, id: number, offset = 220) {
  const strip = await openStrip(pane, id, offset);
  await strip.getByRole("button", { name: "All emoji" }).click();
  const picker = pane.frame.getByRole("dialog", { name: "All emoji" });
  await expect(picker.getByRole("gridcell").first()).toBeVisible();
  return { strip, picker };
}
const reacts = async (pane: MountedPane) =>
  (await pane.verbs()).filter((v) => v.verb === "react").map((v) => v.args);
const lastPrefs = async (pane: MountedPane) =>
  (await pane.verbs()).filter((v) => v.verb === "setPrefs").at(-1)?.args as
    | Record<string, unknown>
    | undefined;
/** Holds the fake engine's `reactGate` or `rollupGate` until `releaseGate`. */
const holdGate = (pane: MountedPane, gate: "reactGate" | "rollupGate") =>
  pane.page.evaluate((g) => {
    const w = window as any;
    w[g] = new Promise<void>((resolve) => (w[g + "Release"] = resolve));
  }, gate);
const releaseGate = (pane: MountedPane, gate: "reactGate" | "rollupGate") =>
  pane.page.evaluate((g) => {
    const w = window as any;
    w[g + "Release"]?.();
    w[g] = undefined;
  }, gate);
const rollups = async (pane: MountedPane) =>
  (await pane.verbs()).filter((v) => v.verb === "inboxRollup").length;

test("chips sit on the bubble's inner top corner, count distinct reactors and show agent faces", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  const theirs = msg(pane, 1);
  const thumbs = theirs.locator(".rx-chip", { hasText: "👍" });
  await expect(thumbs).toHaveAttribute("aria-label", "Reacted 👍 by you and Reviewer");
  await expect(thumbs).toHaveAttribute("aria-pressed", "true");
  await expect(thumbs.locator(".n")).toHaveText("2");
  await expect(thumbs.locator(".rx-faces img")).toHaveAttribute(
    "src",
    "/kit/v1/avatars/builtin-chess/queen.svg",
  );
  const eyes = theirs.locator(".rx-chip", { hasText: "👀" });
  await expect(eyes).toHaveAttribute("aria-pressed", "false");
  await expect(eyes.locator(".n")).toHaveCount(0);
  await expect(eyes.locator(".rx-faces img")).toHaveAttribute("src", rook);
  const [bubble, cluster, text] = await Promise.all([
    rect(pane, '[data-message-id="1"] .bd'),
    rect(pane, '[data-message-id="1"] .ib-rx'),
    rect(pane, '[data-message-id="1"] .bd p'),
  ]);
  expect(Math.abs(cluster.left - (bubble.right - 16))).toBeLessThanOrEqual(1);
  expect(Math.abs(cluster.top - (bubble.top - 15))).toBeLessThanOrEqual(1);
  expect(cluster.bottom).toBeLessThanOrEqual(text.top + 0.5);
  const [mineBubble, mineCluster] = await Promise.all([
    rect(pane, '[data-message-id="2"] .bd'),
    rect(pane, '[data-message-id="2"] .ib-rx'),
  ]);
  expect(Math.abs(mineCluster.right - (mineBubble.left + 16))).toBeLessThanOrEqual(1);
  const check = msg(pane, 2).locator(".rx-chip");
  await expect(check).toHaveAttribute("aria-label", "Reacted ✅ by Builder");
  await expect(check).toHaveAttribute("aria-pressed", "false");
  await expect(msg(pane, 1).locator(".ib-rx")).toHaveAttribute("role", "group");
  // A reaction is not mail: it never draws as a bubble and leaves Needs your reply standing.
  await expect(pane.frame.locator(".ib-msg")).toHaveCount(23);
  await expect(msg(pane, 3).locator(".foot")).toContainText("Needs your reply");
  const log = await pane.frame
    .locator("#log")
    .evaluate((el) => ({ scroll: el.scrollWidth, client: el.clientWidth }));
  expect(log.scroll).toBeLessThanOrEqual(log.client);
});

test("an agent whose session ended takes its face from its agent definition", async ({
  mountPane,
}) => {
  const pane = await mountPane(INBOX_PANE, world());
  await pane.frame.locator(".ib-row", { hasText: "Scout" }).click();
  await expect(msg(pane, 40).locator(".av img")).toHaveAttribute(
    "src",
    "/kit/v1/avatars/builtin-funny/yaml-wizard.svg",
  );
  await expect(pane.frame.locator("#thread-avatar img")).toHaveAttribute(
    "src",
    "/kit/v1/avatars/builtin-funny/yaml-wizard.svg",
  );
});

test("pressing a chip toggles my reaction at once, then reconciles with the engine", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  await holdGate(pane, "reactGate");
  const thumbs = msg(pane, 1).locator(".rx-chip", { hasText: "👍" });
  await thumbs.click();
  await expect(thumbs).toHaveAttribute("aria-pressed", "false");
  await expect(thumbs).toHaveAttribute("aria-label", "Reacted 👍 by Reviewer");
  await expect(thumbs).toHaveClass(/pending/);
  await expect(pane.frame.locator("#react-status")).toHaveText("Removed 👍 on Builder's message");
  expect(await reacts(pane)).toEqual([
    expect.objectContaining({ messageId: 1, emoji: "👍", remove: true }),
  ]);
  await releaseGate(pane, "reactGate");
  await expect(thumbs).not.toHaveClass(/pending/);
  await expect(thumbs).toHaveAttribute("aria-pressed", "false");
  const check = msg(pane, 2).locator(".rx-chip");
  await check.click();
  await expect(check).toHaveAttribute("aria-label", "Reacted ✅ by you and Builder");
  await expect(check.locator(".n")).toHaveText("2");
  await expect(pane.frame.locator("#react-status")).toHaveText("Reacted ✅ on your message");
  // My only reaction leaves with me.
  await msg(pane, 3).locator(".rx-chip").click();
  await expect(msg(pane, 3).locator(".rx-chip")).toHaveCount(0);
  await expect(msg(pane, 3).locator(".foot")).toContainText("Needs your reply");
  expect((await reacts(pane)).slice(1)).toEqual([
    expect.objectContaining({ messageId: 2, emoji: "✅", remove: false }),
    expect.objectContaining({ messageId: 3, emoji: "✅", remove: true }),
  ]);
});

test("a reaction that is not saved says so under the bubble and Retry saves it", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  await pane.page.evaluate(() => ((window as any).reactError = "Machine offline"));
  const strip = await openStrip(pane, 1);
  await strip.getByRole("button", { name: "folded hands" }).click();
  const chip = msg(pane, 1).locator(".rx-chip", { hasText: "🙏" });
  await expect(chip).toHaveClass(/failed/);
  await expect(msg(pane, 1).locator(".foot .rx-err")).toContainText("Couldn't save 🙏.");
  await expect(pane.frame.locator("#composer-error")).toContainText("Reaction not saved");
  await pane.page.evaluate(() => ((window as any).reactError = undefined));
  await msg(pane, 1).locator(".foot .rx-err").getByRole("button", { name: "Retry" }).click();
  await expect(chip).not.toHaveClass(/failed/);
  await expect(chip).toHaveAttribute("aria-pressed", "true");
  await expect(msg(pane, 1).locator(".foot .rx-err")).toHaveCount(0);
  expect(await reacts(pane)).toHaveLength(2);
});

test("a long press opens the strip with focus on its first emoji", async ({ mountPane }) => {
  const pane = await openBuilder(mountPane);
  const bubble = await scrollBubbleTo(pane, 1, 220);
  const box = await bubble.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { x: r.left + 20, y: r.top + r.height / 2 };
  });
  await bubble.dispatchEvent("pointerdown", {
    pointerType: "touch",
    isPrimary: true,
    button: 0,
    clientX: box.x,
    clientY: box.y,
  });
  await pane.page.waitForTimeout(150);
  await expect(msg(pane, 1).locator(".ib-react")).not.toHaveAttribute("data-open", "true");
  const strip = msg(pane, 1).locator(".ib-react");
  await expect(strip).toHaveAttribute("data-open", "true");
  await expect(strip.getByRole("button").first()).toBeFocused();
  await bubble.dispatchEvent("pointerup", { pointerType: "touch", isPrimary: true, button: 0 });
  await expect(strip).toHaveAttribute("data-open", "true");
});

test("the full picker loads local emoji once, searches names and keywords and picks with the keyboard", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  const loads: string[] = [];
  pane.page.on("request", (r) => {
    if (r.url().includes("emoji.json")) loads.push(r.url());
  });
  await openStrip(pane, 1);
  expect(loads).toHaveLength(0);
  const { picker } = await openPicker(pane, 1);
  expect(loads).toHaveLength(1);
  expect(new URL(loads[0]).pathname).toBe("/kit/v1/emoji.json");
  const search = picker.getByRole("searchbox", { name: "Search emoji" });
  await expect(search).toBeFocused();
  const tabs = picker.getByRole("tablist", { name: "Categories" }).getByRole("tab");
  await expect(tabs).toHaveCount(10);
  await expect(tabs.first()).toHaveAttribute("aria-selected", "true");
  await expect(tabs.first()).toHaveAttribute("aria-label", "Recent");
  const recent = picker.getByRole("rowgroup", { name: "Recent" });
  await expect(recent.getByRole("gridcell")).toHaveText(["👍", "✅", "👀", "❤️", "😂", "🙏"]);
  await expect(recent.getByRole("gridcell").first()).toHaveAttribute("aria-pressed", "true");
  await expect(picker.getByRole("rowgroup", { name: "Smileys & emotion" })).toBeVisible();
  // One roving Tab stop across the whole grid.
  await expect(picker.locator('[role="grid"] [tabindex="0"]')).toHaveCount(1);
  await search.fill("party");
  await expect(picker.getByRole("gridcell", { name: "party popper" })).toBeVisible();
  await expect(picker.locator('[role="status"]')).toHaveText(/^\d+ results?$/);
  for (const tab of await tabs.all()) await expect(tab).toHaveAttribute("aria-selected", "false");
  await search.fill("PINATA");
  await expect(picker.getByRole("gridcell", { name: "piñata" })).toBeVisible();
  await search.fill("spreadsheet");
  await expect(picker).toContainText('No emoji for "spreadsheet"');
  await pane.page.keyboard.press("Escape");
  await expect(search).toHaveValue("");
  await expect(picker).toBeVisible();
  await pane.page.keyboard.press("ArrowDown");
  const first = picker.getByRole("gridcell").first();
  await expect(first).toBeFocused();
  await pane.page.keyboard.press("ArrowRight");
  const second = picker.getByRole("gridcell").nth(1);
  await expect(second).toBeFocused();
  const from = (await second.boundingBox())!;
  await pane.page.keyboard.press("ArrowDown");
  const below = await picker.evaluate(() =>
    document.activeElement!.getBoundingClientRect().toJSON(),
  );
  const frame = (await (await pane.frame.frameElement()).boundingBox())!;
  expect(Math.abs(below.left + frame.x - from.x)).toBeLessThanOrEqual(1);
  expect(below.top + frame.y).toBeGreaterThan(from.y);
  await expect(picker.locator('[role="grid"] [tabindex="0"]')).toHaveCount(1);
  // Typing in the grid goes to search.
  await pane.page.keyboard.type("party");
  await expect(search).toBeFocused();
  await expect(search).toHaveValue("party");
  await pane.page.keyboard.press("Enter");
  await expect(picker).toBeHidden();
  await expect
    .poll(() => reacts(pane))
    .toEqual([expect.objectContaining({ messageId: 1, emoji: "🎉", remove: false })]);
  await expect(msg(pane, 1).locator(".rx-chip", { hasText: "🎉" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(pane.frame.locator("#log")).toBeFocused();
  await expect
    .poll(async () => ((await lastPrefs(pane))?.reactionRecents as string[][] | undefined)?.[0])
    .toEqual(["🎉", "party popper", 0]);
  const next = await openStrip(pane, 2);
  await expect(next.getByRole("button").first()).toHaveText("🎉");
  await expect(next.getByRole("button")).toHaveCount(7);
  await next.getByRole("button", { name: "All emoji" }).click();
  await expect(picker.getByRole("gridcell").first()).toBeVisible();
  expect(loads).toHaveLength(1);
  await pane.page.keyboard.press("Escape");
  await expect(next.getByRole("button", { name: "All emoji" })).toBeFocused();
});

test("the skin tone choice applies to the grid and the strip and is remembered", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  const { strip, picker } = await openPicker(pane, 1);
  const tone = picker.getByRole("button", { name: "Skin tone: Default" });
  await expect(tone).toHaveAttribute("aria-expanded", "false");
  await tone.click();
  await expect(picker.getByRole("tablist")).toHaveCount(0);
  const tones = picker.getByRole("radiogroup", { name: "Skin tone" });
  await expect(tones.getByRole("radio")).toHaveCount(6);
  await tones.getByRole("radio", { name: "Medium", exact: true }).click();
  await expect(picker.getByRole("tablist", { name: "Categories" })).toBeVisible();
  await expect(picker.getByRole("button", { name: "Skin tone: Medium" })).toBeVisible();
  await picker.locator("input").fill("thumbs up");
  await expect(picker.getByRole("gridcell", { name: "thumbs up" }).first()).toHaveText("👍🏽");
  await expect.poll(async () => (await lastPrefs(pane))?.skinTone).toBe(3);
  await pane.page.keyboard.press("Escape");
  await pane.page.keyboard.press("Escape");
  await expect(picker).toBeHidden();
  await expect(strip.getByRole("button", { name: "thumbs up" })).toHaveText("👍🏽");
});

test("the full picker flips to the side with room and never covers its message, chips or the composer", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  for (const [id, offset, side] of [
    [12, 8, "below"],
    [3, 420, "above"],
  ] as const) {
    const { picker } = await openPicker(pane, id, offset);
    const [box, message, log, composer] = await Promise.all([
      picker.evaluate((el) => el.getBoundingClientRect().toJSON()),
      rect(pane, `[data-message-id="${id}"]`),
      rect(pane, "#log"),
      rect(pane, "#composer"),
    ]);
    const chips = await msg(pane, id)
      .locator(".ib-rx")
      .evaluateAll((els) => els.map((el) => el.getBoundingClientRect().toJSON()));
    const strip = await rect(pane, `[data-message-id="${id}"] .ib-react`);
    for (const r of [message, strip, ...chips])
      expect(box.bottom <= r.top + 0.5 || box.top >= r.bottom - 0.5).toBe(true);
    if (side === "below") expect(box.top).toBeGreaterThanOrEqual(message.bottom);
    else expect(box.bottom).toBeLessThanOrEqual(strip.top);
    expect(box.bottom).toBeLessThanOrEqual(composer.top);
    expect(box.top).toBeGreaterThanOrEqual(log.top);
    expect(box.left).toBeGreaterThanOrEqual(log.left);
    expect(box.right).toBeLessThanOrEqual(log.right);
    await pane.page.keyboard.press("Escape");
    await pane.page.keyboard.press("Escape");
    await expect(picker).toBeHidden();
  }
});

test("if the emoji data can't load the picker offers Retry and the strip still reacts", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  await pane.page.route("**/kit/v1/emoji.json", (route) => route.fulfill({ status: 500 }));
  const strip = await openStrip(pane, 1);
  await strip.getByRole("button", { name: "All emoji" }).click();
  const picker = pane.frame.getByRole("dialog", { name: "All emoji" });
  await expect(picker).toContainText("Couldn't load emoji.");
  await pane.page.unroute("**/kit/v1/emoji.json");
  await picker.getByRole("button", { name: "Retry" }).click();
  await expect(picker.getByRole("gridcell").first()).toBeVisible();
  await pane.page.keyboard.press("Escape");
  await strip.getByRole("button", { name: "eyes" }).click();
  await expect
    .poll(() => reacts(pane))
    .toEqual([expect.objectContaining({ messageId: 1, emoji: "👀", remove: false })]);
});

test("a reaction on older history outside the snapshot window survives refreshes", async ({
  mountPane,
}) => {
  const pane = await openOldHistory(mountPane, historyRows({}));
  const strip = await openStrip(pane, 5);
  await strip.getByRole("button", { name: "eyes" }).click();
  const chip = msg(pane, 5).locator(".rx-chip", { hasText: "👀" });
  await expect(chip).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => reacts(pane)).toHaveLength(1);
  for (let i = 0; i < 2; i++) {
    const before = await rollups(pane);
    await pane.page.evaluate(() => (window as any).emit("agent-inbox-changed", null));
    await expect.poll(() => rollups(pane)).toBeGreaterThan(before);
    await pane.page.waitForTimeout(150);
    await expect(chip).toHaveAttribute("aria-pressed", "true");
    await expect(chip).not.toHaveClass(/pending/);
  }
});

test("at 300px the picker fits the thread and arrows move by its drawn columns", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane, { ...world(), width: 300 });
  const { picker } = await openPicker(pane, 1);
  const [box, log] = await Promise.all([
    picker.evaluate((el) => el.getBoundingClientRect().toJSON()),
    rect(pane, "#log"),
  ]);
  expect(box.left).toBeGreaterThanOrEqual(log.left);
  expect(box.right).toBeLessThanOrEqual(log.right);
  const grid = await picker
    .locator('[role="grid"]')
    .evaluate((el) => ({ scroll: el.scrollWidth, client: el.clientWidth }));
  expect(grid.scroll).toBeLessThanOrEqual(grid.client);
  const smileys = picker.getByRole("rowgroup", { name: "Smileys & emotion" });
  const firstRow = smileys.getByRole("row").first().getByRole("gridcell");
  const columns = await firstRow.count();
  expect(columns).toBeLessThan(8);
  for (const cell of await firstRow.all()) {
    const r = (await cell.evaluate((el) => el.getBoundingClientRect().toJSON())) as DOMRect;
    expect(r.right).toBeLessThanOrEqual(box.right);
  }
  await smileys.getByRole("gridcell").first().focus();
  await pane.page.keyboard.press("ArrowDown");
  await expect(smileys.getByRole("gridcell").nth(columns)).toBeFocused();
  await pane.page.keyboard.press("End");
  await expect(smileys.getByRole("gridcell").nth(2 * columns - 1)).toBeFocused();
});

test("a tall message keeps the picker clear of it, its chips and the composer", async ({
  mountPane,
}) => {
  const fixtures = world();
  fixtures.height = 720;
  fixtures.inbox = [
    ...fixtures.inbox!,
    row(
      4,
      Array.from({ length: 12 }, (_, n) => `Report line ${n}: the long status that wraps.`).join(
        "\n\n",
      ),
      6,
      { reactions: [{ emoji: "👀", count: 1, reactors: ["builder"] }] },
    ),
  ];
  const pane = await openBuilder(mountPane, fixtures, 900);
  await expect(msg(pane, 4)).toBeVisible();
  const [log, tall] = await Promise.all([rect(pane, "#log"), rect(pane, '[data-message-id="4"]')]);
  expect(log.height).toBeGreaterThan(440);
  expect(log.height).toBeLessThan(560);
  expect(tall.height).toBeGreaterThan(320);
  const { picker } = await openPicker(pane, 4, 40);
  const [box, message, composer, chips, strip, view] = await Promise.all([
    picker.evaluate((el) => el.getBoundingClientRect().toJSON()),
    rect(pane, '[data-message-id="4"]'),
    rect(pane, "#composer"),
    rect(pane, '[data-message-id="4"] .ib-rx'),
    rect(pane, '[data-message-id="4"] .ib-react'),
    rect(pane, "#log"),
  ]);
  for (const r of [message, chips, strip])
    expect(box.bottom <= r.top + 0.5 || box.top >= r.bottom - 0.5).toBe(true);
  expect(box.bottom).toBeLessThanOrEqual(composer.top);
  expect(box.top).toBeGreaterThanOrEqual(view.top);
  expect(box.height).toBeGreaterThanOrEqual(240);
});

test("removing a chip from the keyboard keeps focus on a chip, else on the message", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  await pane.upsert(
    "inbox",
    row(2, "Ship it once the theme check is green.", 10, {
      ...outgoing,
      reactions: [
        { emoji: "✅", count: 1, reactors: ["builder"] },
        { emoji: "🙏", count: 1, reactors: ["human"] },
      ],
    }),
  );
  const pray = msg(pane, 2).locator(".rx-chip", { hasText: "🙏" });
  await expect(pray).toBeVisible();
  await pray.focus();
  await pane.page.keyboard.press("Enter");
  await expect(pray).toHaveCount(0);
  await expect(msg(pane, 2).locator(".rx-chip", { hasText: "✅" })).toBeFocused();
  const only = msg(pane, 3).locator(".rx-chip");
  await only.focus();
  await pane.page.keyboard.press("Enter");
  await expect(only).toHaveCount(0);
  await expect(msg(pane, 3).locator(".ib-react button").first()).toBeFocused();
});

test("choosing a category moves the grid's keyboard entry to that category", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  const { picker } = await openPicker(pane, 1);
  await picker.getByRole("tab", { name: "Food & drink" }).click();
  const first = picker
    .getByRole("rowgroup", { name: "Food & drink" })
    .getByRole("gridcell")
    .first();
  await expect(first).toHaveAttribute("tabindex", "0");
  await expect(picker.locator('[role="grid"] [tabindex="0"]')).toHaveCount(1);
  await pane.page.keyboard.press("Tab");
  await expect(first).toBeFocused();
});

test("a multi-person emoji takes its explicit tone variant in the grid and the reaction", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  const { picker } = await openPicker(pane, 1);
  await picker.getByRole("button", { name: "Skin tone: Default" }).click();
  await picker
    .getByRole("radiogroup", { name: "Skin tone" })
    .getByRole("radio", { name: "Medium", exact: true })
    .click();
  const search = picker.getByRole("searchbox", { name: "Search emoji" });
  await search.fill("people holding hands");
  await expect(picker.getByRole("gridcell", { name: "people holding hands" })).toHaveText("🧑🏽‍🤝‍🧑🏽");
  await search.press("Enter");
  await expect
    .poll(() => reacts(pane))
    .toEqual([expect.objectContaining({ messageId: 1, emoji: "🧑🏽‍🤝‍🧑🏽", remove: false })]);
});

test("in a 180px log the picker fits, its tabs and tones stay usable and arrows match the columns", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane, { ...world(), width: 180 });
  const log = await rect(pane, "#log");
  expect(log.width).toBeLessThanOrEqual(190);
  const { picker } = await openPicker(pane, 1);
  const box = await picker.evaluate((el) => el.getBoundingClientRect().toJSON());
  expect(box.left).toBeGreaterThanOrEqual(log.left);
  expect(box.right).toBeLessThanOrEqual(log.right);
  for (const part of ['[role="grid"]', '[role="tablist"]', ".ep-top", ".ep-foot"]) {
    const r = await picker
      .locator(part)
      .evaluate((el) => el.getBoundingClientRect().toJSON() as DOMRect);
    expect(r.right).toBeLessThanOrEqual(box.right + 0.5);
  }
  const grid = await picker
    .locator('[role="grid"]')
    .evaluate((el) => ({ scroll: el.scrollWidth, client: el.clientWidth }));
  expect(grid.scroll).toBeLessThanOrEqual(grid.client);
  const flags = picker.getByRole("tab", { name: "Flags" });
  await flags.click();
  await expect(flags).toHaveAttribute("aria-selected", "true");
  await picker.getByRole("button", { name: /^Skin tone:/ }).click();
  const dark = picker
    .getByRole("radiogroup", { name: "Skin tone" })
    .getByRole("radio", { name: "Dark", exact: true });
  await dark.click();
  await expect(picker.getByRole("button", { name: "Skin tone: Dark" })).toBeVisible();
  const smileys = picker.getByRole("rowgroup", { name: "Smileys & emotion" });
  await picker.getByRole("tab", { name: "Smileys & emotion" }).click();
  const columns = await smileys.getByRole("row").first().getByRole("gridcell").count();
  expect(columns).toBeGreaterThanOrEqual(1);
  expect(columns).toBeLessThan(4);
  await smileys.getByRole("gridcell").first().focus();
  await pane.page.keyboard.press("ArrowDown");
  await expect(smileys.getByRole("gridcell").nth(columns)).toBeFocused();
});

test("crossing a column threshold keeps the focused emoji and the grid's place", async ({
  mountPane,
}) => {
  const pane = await openBuilder(mountPane);
  const { picker } = await openPicker(pane, 1);
  await picker.getByRole("tab", { name: "Travel & places" }).click();
  const travel = picker.getByRole("rowgroup", { name: "Travel & places" });
  const target = travel.getByRole("gridcell").nth(10);
  await target.focus();
  const name = await target.getAttribute("aria-label");
  const before = await picker.locator('[role="grid"]').evaluate((el) => el.scrollTop);
  expect(before).toBeGreaterThan(0);
  const columns = async () =>
    picker
      .getByRole("rowgroup", { name: "Smileys & emotion" })
      .getByRole("row")
      .first()
      .getByRole("gridcell")
      .count();
  const wide = await columns();
  await (
    await pane.frame.frameElement()
  ).evaluate((el) => {
    (el as HTMLElement).style.width = "300px";
  });
  await expect.poll(columns).toBeLessThan(wide);
  await expect(picker.getByRole("gridcell", { name: name!, exact: true }).first()).toBeFocused();
  const focused = await picker.evaluate(() => {
    const cell = document.activeElement!.getBoundingClientRect();
    const grid = document.querySelector('[role="grid"]')!.getBoundingClientRect();
    return { cell: cell.toJSON(), grid: grid.toJSON() };
  });
  expect(focused.cell.top).toBeGreaterThanOrEqual(focused.grid.top);
  expect(focused.cell.bottom).toBeLessThanOrEqual(focused.grid.bottom);
  expect(await picker.locator('[role="grid"]').evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
});

test("an agent's first reaction on an old loaded row outside the window shows from the change event", async ({
  mountPane,
}) => {
  const rows = historyRows({ reactions: [] });
  const pane = await openOldHistory(mountPane, rows);
  const reads = async () => (await pane.verbs()).filter((v) => v.verb === "inbox").length;
  const before = await reads();
  await builderReacts(pane, rows[4], [5]);
  await expect(msg(pane, 5).locator(".rx-chip", { hasText: "👀" })).toHaveAttribute(
    "aria-label",
    "Reacted 👀 by Builder",
  );
  // One bounded re-read of the changed row, not a history reload.
  expect((await reads()) - before).toBeLessThanOrEqual(2);
});

test("a refresh in flight while the pane hides keeps the named row for after it shows", async ({
  mountPane,
}) => {
  const rows = historyRows({ reactions: [] });
  const pane = await openOldHistory(mountPane, rows);
  await holdGate(pane, "rollupGate");
  const before = await rollups(pane);
  await builderReacts(pane, rows[4], [5]);
  await expect.poll(() => rollups(pane)).toBeGreaterThan(before);
  await pane.show(false);
  await releaseGate(pane, "rollupGate");
  await pane.page.waitForTimeout(300);
  await pane.show(true);
  await expect(msg(pane, 5).locator(".rx-chip", { hasText: "👀" })).toHaveAttribute(
    "aria-label",
    "Reacted 👀 by Builder",
  );
});

test("an old row's reaction shows once the committed change names it, and not before", async ({
  mountPane,
}) => {
  const rows = historyRows({ reactions: [] });
  const pane = await openOldHistory(mountPane, rows);
  const chip = msg(pane, 5).locator(".rx-chip", { hasText: "👀" });
  const before = await rollups(pane);
  await builderReacts(pane, rows[4], []);
  await expect.poll(() => rollups(pane)).toBeGreaterThan(before);
  await pane.page.waitForTimeout(300);
  await expect(chip).toHaveCount(0);
  await pane.page.evaluate((t) => (window as any).inboxChanged(t, [5]), team);
  await expect(chip).toHaveAttribute("aria-label", "Reacted 👀 by Builder");
});
