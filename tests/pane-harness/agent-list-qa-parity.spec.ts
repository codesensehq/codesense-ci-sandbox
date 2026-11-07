// SPDX-License-Identifier: MIT
// SV-AGP-9 QA: interaction parity under a 100ms live delta stream, 50 agents, both browsers.
// Soft assertions so every row of design/SV-AGP-9/PARITY-QA.md gets a verdict in one run.
import { test, expect, type MountedPane } from "./harness";
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
import { hitStability } from "./agent-list-hit-stability";
import { qaWorld, startStream, stopStream } from "./agent-list-qa-world";

const main = (pane: MountedPane, ref: string) =>
  pane.frame.locator(`[id="agent-row:${ref}"] .al-main`);
async function command(pane: MountedPane, id: string) {
  await pane.page.locator("iframe").evaluate((el: HTMLIFrameElement, c: string) => {
    el.contentWindow!.postMessage({ type: "codesense:command", id: c }, "*");
  }, id);
}
async function mount(mountPane: any, arrange = "teams", extra: Record<string, unknown> = {}) {
  const world = { ...qaWorld(arrange), ...extra };
  const pane: MountedPane = await mountPane(AGENT_LIST_PANE, world);
  await expect(pane.frame.locator("#count")).toHaveText("50");
  await startStream(pane.page, world);
  return { pane, world };
}

test.describe.configure({ timeout: 90_000 });
async function softPoll(fn: () => Promise<boolean>, message: string, timeout = 3000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeout) {
    if (await fn()) return;
    await new Promise((r) => setTimeout(r, 100));
  }
  expect.soft(false, message).toBe(true);
}
async function admitTeams(pane: MountedPane) {
  await pane.page.evaluate(async () => {
    const { useOrgStore } = await import("/src/shared/stores/org-store.ts" as string);
    useOrgStore.setState({
      orgs: [{ slug: "codesense", teams: [{ name: "riverside" }, { name: "core" }] }],
    });
  });
}
async function report(info: any, name: string, data: unknown) {
  const body = JSON.stringify(data, null, 1);
  console.log(`QA[${info.project.name}] ${name}: ${body}`);
  await info.attach(name, { body, contentType: "application/json" });
}

test("QA pins: section order, +N more pinned, menu pin, hover pin", async ({ mountPane }, info) => {
  const { pane } = await mount(mountPane, "teams", { width: 268 });
  const { frame, page } = pane;
  const navigations: string[] = [];
  const t0 = Date.now();
  const note = (line: string) => (navigations.push(line), console.log(`QA-NAV ${line}`));
  page.on("framenavigated", (f) =>
    note(
      `${Date.now() - t0}ms ${f === page.mainFrame() ? "HOST" : "frame"} ${f.url().slice(0, 80)}`,
    ),
  );
  const step = (name: string) => note(`${Date.now() - t0}ms step ${name}`);
  await expect.soft(frame.getByText("Pinned", { exact: true }).first()).toBeVisible();
  const order = await frame
    .locator('[id^="agent-row:pinned:"]')
    .evaluateAll((els) => els.map((el) => el.id.replace("agent-row:pinned:", "")));
  expect
    .soft(order, "pinned order = manual pinnedRefs order, first five")
    .toEqual(["r8", "r15", "r1", "r30", "r12"]);
  const more = frame.getByRole("button", { name: "+2 more pinned" });
  await expect.soft(more, "+N more pinned").toBeVisible();
  await more.click();
  await expect.soft(frame.locator('[id^="agent-row:pinned:"]')).toHaveCount(7);
  await expect.soft(frame.getByRole("button", { name: "Show fewer pinned" })).toBeVisible();
  // Native draws a hover Pin action at >= 260px.
  await main(pane, "r9").hover();
  const hoverPin = await frame
    .locator('[id="agent-row:r9"]')
    .getByRole("button", { name: /^(Pin|Unpin)\b/ })
    .count();
  expect.soft(hoverPin, "hover Pin control at 268px (native T:3424)").toBe(1);
  // The pane writes canonical pins through setUiState("ui:dock", {pins}).
  const storePins = async () => {
    const last = (await pane.verbs())
      .filter(
        (v) =>
          (v.verb === "setUiState" && (v.args as any)?.patch?.pins !== undefined) ||
          (v.verb === "setUiState" && JSON.stringify(v.args).includes('"pins"')),
      )
      .at(-1);
    return last ? JSON.stringify(last.args) : "no pin write";
  };
  step("before pinsAtStart");
  const pinsAtStart = await frame
    .locator(
      '[id="agent-row:r8"] .al-unpin:not([hidden]), [id="agent-row:pinned:r8"] .al-unpin:not([hidden])',
    )
    .count();
  expect.soft(pinsAtStart, "canonical world pin shows its pinned glyph").toBeGreaterThan(0);
  let hoverPinResult: string | null = null,
    glyphUnpinResult: string | null = null;
  if (hoverPin) {
    step("hover r16");
    await frame
      .locator('[id="agent-row:r9"] .al-main')
      .hover({ timeout: 3000 })
      .catch(() => {});
    step("hovered r16");
    const pinBtn = frame.locator('[id="agent-row:r9"]').getByRole("button", { name: /^Pin\b/ });
    expect.soft(await pinBtn.count(), "hover Pin button on r9").toBe(1);
    if (await pinBtn.count()) {
      await pinBtn.click();
      await page.waitForTimeout(800);
      hoverPinResult = await storePins();
      expect.soft(hoverPinResult, "hover Pin writes the canonical pin").toMatch(/release-notes/);
    }
  }
  // Unpin from the Pinned group's glyph.
  const glyph = frame
    .locator(
      '[id="agent-row:pinned:r8"] .al-unpin:not([hidden]), [id="agent-row:r8"] .al-unpin:not([hidden])',
    )
    .first();
  expect.soft(await glyph.count(), "pinned glyph is a clickable Unpin").toBe(1);
  if (await glyph.count()) {
    await glyph.click();
    await page.waitForTimeout(800);
    glyphUnpinResult = await storePins();
    expect
      .soft(glyphUnpinResult, "glyph Unpin removes the canonical pin")
      .not.toMatch(/small-fixes/);
  }
  await main(pane, "r5").click({ button: "right" });
  const menu = page.getByRole("menu");
  await expect.soft(menu).toBeVisible();
  const menuText = (await menu.innerText().catch(() => "")).replace(/\n+/g, " | ");
  expect.soft(menuText, "row menu has Pin").toMatch(/\bPin\b/);
  const pinItem = page.getByRole("menuitem", { name: /^Pin/ });
  if (await pinItem.count()) await pinItem.first().click();
  else await page.keyboard.press("Escape");
  await page.waitForTimeout(1200);
  // The harness's agentList topic is a static fixture, so check the host's pin store instead.
  const pinnedNow = await page.evaluate(async () => {
    const { useViewerStore } = await import("/src/shared/stores/viewer-store.ts" as string);
    return JSON.stringify(useViewerStore.getState().agentPins ?? null);
  });
  const pinWrites = await storePins();
  expect
    .soft(`${pinnedNow} ${pinWrites}`, "menu Pin reaches the canonical pins")
    .toMatch(/sv-inp-13-lead/);
  const ui = await page
    .evaluate(() => (window as any).uiState?.("ui:dock") ?? null)
    .catch(() => null);
  const ticks = await stopStream(page);
  await report(info, "pins", {
    navigations,
    pinWrites,
    pinsAtStart,
    hoverPinResult,
    glyphUnpinResult,
    pinnedNow,
    order,
    menuText,
    hoverPin,
    ui,
    ticks,
    verbs: await pane.verbs(),
  });
  expect.soft(ticks).toBeGreaterThan(10);
});

test("QA drag: card drag line, drop, refused, Option+arrow under stream", async ({
  mountPane,
}, info) => {
  const { pane, world } = await mount(mountPane, "teams");
  const { frame, page } = pane;
  const notes: Record<string, unknown> = {};
  // The host moves only cards with a tray pill: give every workspace one.
  await page.evaluate(
    (order) => {
      (window as any).viewer.setState({
        workspaces: Object.fromEntries(
          Object.entries(order).map(([team, ids]: any) => [
            team,
            ids.map((id: string) => ({ id, kind: "generic", status: "ready", cwd: "/" + id })),
          ]),
        ),
        trayBaseOrderByTeam: order,
      });
    },
    world.topics.agentList.find((r: any) => r.rowType === "state").pane.workspaceOrder,
  );
  // Fold every card so several cards are in view, then drag the first card down past two others.
  await frame.locator(".al-team-head").first().focus();
  await command(pane, "collapse-all");
  await page.waitForTimeout(400);
  await command(pane, "expand-all");
  await page.waitForTimeout(400);
  const wsFolds = await frame.locator(".al-ws-head").count();
  for (let i = 0; i < wsFolds; i++) {
    const h = frame.locator(".al-ws-head").nth(i);
    if ((await h.getAttribute("aria-expanded")) === "true") await h.click();
  }
  await page.waitForTimeout(400);
  notes.cards = await frame.locator(".al-ws-head .al-ws-name").allTextContents();
  const head = frame.locator(".al-ws-head").first();
  const box = (await head.boundingBox())!;
  await page.mouse.move(box.x + 60, box.y + box.height / 2);
  await page.mouse.down();
  for (let i = 1; i <= 12; i++) {
    await page.mouse.move(box.x + 60, box.y + box.height / 2 + i * 8);
    await page.waitForTimeout(40);
  }
  notes.lineDuring = await frame.locator(".al-drop-line").count();
  expect.soft(notes.lineDuring, "drop line shows while dragging under stream").toBeGreaterThan(0);
  await page.waitForTimeout(600);
  notes.lineStill = await frame.locator(".al-drop-line").count();
  expect.soft(notes.lineStill, "drop line survives live deltas mid-drag").toBeGreaterThan(0);
  await page.mouse.up();
  await softPoll(
    async () => (await pane.verbs()).some((v) => v.verb === "moveWorkspace"),
    "card drop sends moveWorkspace",
  );
  // Keyboard move on the second card.
  await page.waitForTimeout(600);
  const before = (await pane.verbs()).filter((v) => v.verb === "moveWorkspace").length;
  const second = frame.locator(".al-ws-head").nth(1);
  const name = await second.locator(".al-ws-name").textContent();
  await second.focus();
  await page.keyboard.press("Alt+ArrowDown");
  await softPoll(
    async () => (await pane.verbs()).filter((v) => v.verb === "moveWorkspace").length > before,
    "Option+ArrowDown sends a move",
  );
  notes.notice = await frame.locator("#al-move-notice").textContent();
  await page.waitForTimeout(600);
  notes.focusAfterMove = await frame.evaluate(
    () => document.activeElement?.closest(".al-ws-head")?.querySelector(".al-ws-name")?.textContent,
  );
  expect.soft(notes.focusAfterMove, "focus stays on the moved card").toBe(name);
  // Refused: a search is on.
  await frame.locator("#al-search").fill("QA");
  await page.waitForTimeout(300);
  await frame.locator(".al-ws-head").first().focus();
  await page.keyboard.press("Alt+ArrowDown");
  await expect
    .soft(frame.locator("#al-move-notice"), "refusal announced")
    .toHaveText(/search|filter/i);
  // Refused pointer drag while filtered: no line, a notice.
  const fh = (await frame.locator(".al-ws-head").first().boundingBox())!;
  await page.mouse.move(fh.x + 60, fh.y + fh.height / 2);
  await page.mouse.down();
  for (let i = 1; i <= 6; i++) await page.mouse.move(fh.x + 60, fh.y + fh.height / 2 + i * 10);
  notes.refusedLine = await frame.locator(".al-drop-line").count();
  await page.mouse.up();
  notes.refusedNotice = await frame.locator("#al-move-notice").textContent();
  expect.soft(notes.refusedLine, "refused drag draws no drop line").toBe(0);
  const ticks = await stopStream(page);
  await report(info, "drag", {
    ...notes,
    ticks,
    acts: (await pane.verbs()).filter((v) => v.verb === "moveWorkspace"),
  });
});

test("QA header: view select, group, search, filter dropdown under stream", async ({
  mountPane,
}, info) => {
  const { pane } = await mount(mountPane, "teams");
  const { frame, page } = pane;
  const notes: Record<string, unknown> = {};
  await frame.locator("#al-arrange").selectOption("manager");
  await expect.soft(frame.locator("[data-manager-view]").first()).toBeVisible();
  await frame.locator("#al-arrange").selectOption("status");
  await page.waitForTimeout(300);
  notes.statusHeadings = await frame
    .locator(".al-item")
    .evaluateAll((els) =>
      els.filter((e) => !e.querySelector(".al-row")).map((e) => e.textContent?.trim().slice(0, 30)),
    );
  notes.statusFirstRows = await frame
    .locator(".al-item")
    .evaluateAll((els) =>
      els
        .slice(0, 14)
        .map(
          (e) => (e.getAttribute("data-row-id") || "") + " " + e.textContent?.trim().slice(0, 30),
        ),
    );
  await frame.locator("#al-arrange").selectOption("teams");
  notes.groupOptions = await frame.locator("#al-scope option").allTextContents();
  // Search.
  await frame.locator("#al-search").fill("Merge waiter");
  await page.waitForTimeout(400);
  const agentRows = frame.locator(".al-row:not([id*=pinned])");
  await expect.soft(agentRows).toHaveCount(1);
  await frame.locator("#al-search").press("Escape");
  await expect.soft(frame.locator("#al-search")).toHaveValue("");
  await expect.soft(frame.locator("#al-search"), "Esc clear keeps focus in search").toBeFocused();
  // Filter dropdown.
  const filter = frame.locator("#al-filter");
  await filter.click();
  const menu = frame.locator("#al-menu");
  await expect.soft(menu).toBeVisible();
  notes.focusInMenuOnOpen = await frame.evaluate(
    () => !!document.getElementById("al-menu")?.contains(document.activeElement),
  );
  const listTop0 = await frame.locator("#list").evaluate((el) => el.scrollTop);
  const menuNode = await menu.elementHandle();
  await menu.hover();
  await page.mouse.wheel(0, 400);
  await page.waitForTimeout(1200);
  notes.menuScroll = await frame.evaluate(() => {
    const m = document.getElementById("al-menu")!;
    return { scrollTop: m.scrollTop, scrollHeight: m.scrollHeight, clientHeight: m.clientHeight };
  });
  const listTop1 = await frame.locator("#list").evaluate((el) => el.scrollTop);
  expect.soft(listTop1, "wheel inside filter does not scroll the list").toBe(listTop0);
  notes.menuNodeKept = await menuNode!.evaluate((n) => n.isConnected);
  notes.focusInMenuAfterStream = await frame.evaluate(
    () => !!document.getElementById("al-menu")?.contains(document.activeElement),
  );
  expect.soft(notes.focusInMenuAfterStream, "focus stays in menu under deltas").toBe(true);
  await page.keyboard.press("Escape");
  await expect.soft(menu).toBeHidden();
  await expect.soft(filter, "Esc returns focus to Filter").toBeFocused();
  // The trigger toggles it closed (a mouse click, not a keyboard press).
  await filter.click();
  await expect.soft(menu).toBeVisible();
  await filter.click();
  await page.waitForTimeout(300);
  notes.triggerTogglesClosed = !(await menu.isVisible());
  expect.soft(notes.triggerTogglesClosed, "clicking the open Filter trigger closes it").toBe(true);
  if (await menu.isVisible()) await page.keyboard.press("Escape");
  // Outside click inside the pane.
  await filter.click();
  await expect.soft(menu).toBeVisible();
  await frame.locator("#al-title").click();
  await expect.soft(menu, "outside click in pane closes").toBeHidden();
  // Outside click in the host (focus leaves the frame).
  await filter.click();
  await expect.soft(menu).toBeVisible();
  await page
    .locator("#terminal")
    .click({ force: true })
    .catch(() => page.mouse.click(5, 5));
  await page.waitForTimeout(300);
  notes.menuAfterHostClick = await menu.isVisible();
  expect
    .soft(notes.menuAfterHostClick, "click outside the pane (host) closes the filter")
    .toBe(false);
  if (await menu.isVisible()) await filter.click();
  // Blur by Tab out.
  await filter.click();
  await expect.soft(menu).toBeVisible();
  for (let i = 0; i < 40 && (await menu.isVisible()); i++) await page.keyboard.press("Tab");
  await expect.soft(menu, "Tab out closes").toBeHidden();
  const ticks = await stopStream(page);
  await report(info, "header", { ...notes, ticks });
});

test("QA tally filters keep focus and filter under stream", async ({ mountPane }, info) => {
  const { pane } = await mount(mountPane, "teams");
  const { frame, page } = pane;
  const results: Record<string, unknown> = {};
  for (const status of ["needs", "working", "idle"]) {
    const button = frame.locator(`#al-tally [data-status="${status}"]`);
    if (!(await button.count())) {
      results[status] = "missing";
      expect.soft(0, `tally ${status} present`).toBe(1);
      continue;
    }
    await button.focus();
    await page.keyboard.press("Enter");
    await page.waitForTimeout(1200);
    await expect
      .soft(frame.locator(`#al-tally [data-status="${status}"]`), `${status} keeps focus`)
      .toBeFocused();
    const pressed = await frame
      .locator(`#al-tally [data-status="${status}"]`)
      .getAttribute("aria-pressed");
    const rows = await frame.locator(".al-row:not([id*=pinned])").evaluateAll((els) =>
      els.map((e: any) => ({
        id: e.id,
        needs: e.hasAttribute("data-needs"),
        state: e.querySelector(".al-state")?.textContent,
      })),
    );
    results[status] = { pressed, rows: rows.slice(0, 40) };
    if (status === "needs")
      expect
        .soft(
          rows.every((r) => r.needs === true),
          "waiting = needs rows only",
        )
        .toBe(true);
    if (status === "working")
      expect
        .soft(
          rows.filter((r) => /Offline/.test(r.state || "")).map((r) => r.id),
          "Working filter has no offline rows",
        )
        .toEqual([]);
    await page.keyboard.press("Enter");
    await page.waitForTimeout(300);
    await expect
      .soft(
        frame.locator(`#al-tally [data-status="${status}"]`),
        `${status} toggle-off keeps focus`,
      )
      .toBeFocused();
  }
  // Pointer: click waiting while the stream runs, hit stays.
  results.hit = await hitStability(pane.frame, '#al-tally [data-status="working"]', 1500);
  results.hitNeeds = await hitStability(pane.frame, '#al-tally [data-status="needs"]', 1500);
  for (const k of ["hit", "hitNeeds"])
    expect.soft((results as any)[k].misses, `${k} stable`).toBe(0);
  const ticks = await stopStream(page);
  await report(info, "tally", { results, ticks });
});

for (const width of [236, 268])
  test(`QA row gestures under stream at ${width}px`, async ({ mountPane }, info) => {
    const { pane } = await mount(mountPane, "teams", { width });
    const { frame, page } = pane;
    const notes: Record<string, unknown> = {};
    const row = main(pane, "r9");
    await row.click();
    await softPoll(
      async () =>
        (await pane.verbs()).some(
          (v) => v.verb === "gotoAgent" && (v.args as any).agentId === "s9",
        ),
      "click -> gotoAgent s9",
    );
    await row.focus();
    for (const [key, kind] of [
      ["Enter", "agent-quick-send"],
      ["Space", "agent-quick-send"],
      ["q", "agent-quick-send"],
      ["m", "agent-inbox-chat"],
    ]) {
      await main(pane, "r9").focus();
      await page.keyboard.press(key);
      const popup = page.getByTestId(kind);
      await expect.soft(popup, `${key} opens ${kind}`).toBeVisible();
      await page.waitForTimeout(400);
      await page.keyboard.press("Escape");
      await expect.soft(popup).toBeHidden();
      await expect.soft(main(pane, "r9"), `${key}: focus returns to row after popup`).toBeFocused();
    }
    await main(pane, "r9").click({ modifiers: ["Alt"] });
    await expect.soft(page.getByTestId("agent-quick-send"), "Alt-click quick view").toBeVisible();
    await page.keyboard.press("Escape");
    // Hover actions stay hittable while the hovered row streams (s0 is streamed).
    await main(pane, "r0").hover();
    notes.quickHit = await hitStability(pane.frame, '[id="agent-row:r0"] .al-quick');
    notes.msgHit = await hitStability(pane.frame, '[id="agent-row:r0"] .al-messages');
    notes.mainHit = await hitStability(pane.frame, '[id="agent-row:r0"] .al-main');
    for (const k of ["quickHit", "msgHit", "mainHit"]) {
      const h = notes[k] as any;
      expect
        .soft(h.found && h.misses === 0 && h.replaced === 0, `${k} stable: ${JSON.stringify(h)}`)
        .toBe(true);
    }
    await frame.locator('[id="agent-row:r0"] .al-quick').click();
    await expect.soft(page.getByTestId("agent-quick-send"), "hover Quick view click").toBeVisible();
    await page.keyboard.press("Escape");
    await main(pane, "r0").hover();
    await frame.locator('[id="agent-row:r0"] .al-messages').click();
    await expect.soft(page.getByTestId("agent-inbox-chat"), "hover Messages click").toBeVisible();
    await page.keyboard.press("Escape");
    // Messages badge (needs-you mail rows: index % 6 === 1): a real mouse aims at its centre.
    const badges = frame.locator(".al-row .al-msgs");
    notes.badgeCount = await badges.count();
    expect.soft(notes.badgeCount, "messages badge on unread rows").toBeGreaterThan(0);
    if (await badges.count()) {
      const badgeRow = await badges.first().evaluate((b) => b.closest(".al-row")!.id);
      const bb = (await badges.first().boundingBox())!;
      await page.mouse.move(bb.x + bb.width / 2, bb.y + bb.height / 2);
      await page.waitForTimeout(300);
      notes.badgeUnderPointer = await frame.evaluate(
        ([id]) => {
          const b = document.querySelector(`[id="${id}"] .al-msgs`) as HTMLElement | null;
          if (!b) return "badge gone";
          const r = b.getBoundingClientRect();
          const at = document.elementFromPoint(
            r.left + r.width / 2,
            r.top + r.height / 2,
          ) as HTMLElement | null;
          const control = at?.closest("button, [role=button]") as HTMLElement | null;
          return {
            badgeVisible: getComputedStyle(b).visibility,
            hit: control?.className,
            label: control?.getAttribute("aria-label"),
          };
        },
        [badgeRow] as const,
      );
      const pinsBefore = (await pane.verbs()).length;
      await page.mouse.down();
      await page.mouse.up();
      await page.waitForTimeout(500);
      notes.badgeClickVerbs = (await pane.verbs())
        .slice(pinsBefore)
        .map((v) => `${v.verb}:${JSON.stringify(v.args).slice(0, 80)}`);
      await expect
        .soft(
          page.getByTestId("agent-inbox-chat"),
          `mouse click on the badge opens chat (hit ${JSON.stringify(notes.badgeUnderPointer)})`,
        )
        .toBeVisible();
      await page.keyboard.press("Escape");
    }
    // via link.
    const via = frame.locator('[id="agent-row:r5"] .al-via');
    if (await via.count()) {
      await via.click();
      await softPoll(
        async () =>
          (await pane.verbs()).some(
            (v) => v.verb === "gotoAgent" && (v.args as any).agentId === "s0",
          ),
        "via -> gotoAgent launcher s0",
      );
    } else expect.soft(0, "via on r5").toBe(1);
    // Double-click: native has no handler (two row clicks).
    const gotoBefore = (await pane.verbs()).filter((v) => v.verb === "gotoAgent").length;
    await main(pane, "r9").dblclick();
    await page.waitForTimeout(400);
    const after = await pane.verbs();
    notes.dblclickVerbs = after.slice(-3);
    notes.dblclickGotos = after.filter((v) => v.verb === "gotoAgent").length - gotoBefore;
    // Context menu.
    await main(pane, "r9").click({ button: "right" });
    await expect.soft(page.getByRole("menu"), "right-click host menu").toBeVisible();
    notes.menu = (
      await page
        .getByRole("menu")
        .innerText()
        .catch(() => "")
    ).replace(/\n+/g, " | ");
    await page.waitForTimeout(800);
    await expect.soft(page.getByRole("menu"), "menu stays open under deltas").toBeVisible();
    await page.keyboard.press("Escape");
    await expect.soft(main(pane, "r9"), "menu Esc returns focus").toBeFocused();
    await main(pane, "r9").focus();
    await command(pane, "open-row-menu");
    await expect.soft(page.getByRole("menu"), "open-row-menu command").toBeVisible();
    await page.keyboard.press("Escape");
    const ticks = await stopStream(page);
    await report(info, `gestures-${width}`, {
      ...notes,
      ticks,
      verbs: (await pane.verbs()).map((v) => v.verb),
    });
  });

test("QA folds and launch under stream", async ({ mountPane }, info) => {
  const { pane } = await mount(mountPane, "teams");
  const { frame, page } = pane;
  const team = frame.locator(".al-team-head").first();
  const teamExpanded = await team.getAttribute("aria-expanded");
  await team.click();
  await page.waitForTimeout(300);
  expect
    .soft(await team.getAttribute("aria-expanded"), "team chevron folds")
    .not.toBe(teamExpanded);
  await team.focus();
  await page.keyboard.press("ArrowRight");
  await page.waitForTimeout(300);
  await expect
    .soft(frame.locator(".al-team-head").first(), "ArrowRight unfolds team")
    .toHaveAttribute("aria-expanded", "true");
  await expect
    .soft(frame.locator(".al-team-head").first(), "focus kept on team head")
    .toBeFocused();
  const ws = frame.locator(".al-ws-head").filter({ hasText: "SV-AGP-9" }).first();
  await ws.focus();
  await page.keyboard.press("ArrowLeft");
  await page.waitForTimeout(400);
  await expect
    .soft(
      frame.locator(".al-ws-head").filter({ hasText: "SV-AGP-9" }).first(),
      "ArrowLeft folds workspace",
    )
    .toHaveAttribute("aria-expanded", "false");
  await expect
    .soft(
      frame.locator(".al-ws-head").filter({ hasText: "SV-AGP-9" }).first(),
      "focus kept on ws head",
    )
    .toBeFocused();
  await page.keyboard.press("ArrowRight");
  // Crew fold (Manager view).
  await frame.locator("#al-arrange").selectOption("manager");
  const crew = frame.locator(".al-fold").first();
  await expect.soft(crew).toBeVisible();
  const crewLabel = await crew.getAttribute("aria-label");
  await crew.click();
  await page.waitForTimeout(400);
  expect
    .soft(
      await frame.locator(".al-fold").first().getAttribute("aria-label"),
      "crew chevron toggles",
    )
    .not.toBe(crewLabel);
  await frame.locator(".al-fold").first().click();
  await frame.locator(".al-row .al-main").first().focus();
  await command(pane, "collapse-all");
  await page.waitForTimeout(500);
  const folded = await frame.locator(".al-row").count();
  await command(pane, "expand-all");
  await page.waitForTimeout(500);
  const expanded = await frame.locator(".al-row").count();
  expect.soft(folded, "collapse-all folds rows").toBeLessThan(expanded);
  await frame.locator("#al-arrange").selectOption("teams");
  // Launch.
  await admitTeams(pane);
  await frame.locator("#al-launch").click();
  await frame
    .locator(".al-team .al-launch, .al-team-head ~ .al-launch")
    .first()
    .click()
    .catch(() => {});
  await frame
    .locator(".al-ws-card .al-launch")
    .first()
    .click({ force: true })
    .catch(() => {});
  await page.waitForTimeout(500);
  const launches = (await pane.verbs()).filter((v) => v.verb === "openLauncher");
  expect.soft(launches.length, "header, team and workspace Launch").toBeGreaterThanOrEqual(3);
  const ticks = await stopStream(page);
  await report(info, "folds", { folded, expanded, launches, ticks });
});

test("QA states: needs-you, blocked, subagents, offline, crashed, ended", async ({
  mountPane,
}, info) => {
  const { pane } = await mount(mountPane, "teams", { height: 2600 });
  const { frame, page } = pane;
  const facts = await frame.locator(".al-row:not([id*=pinned])").evaluateAll((els) =>
    Object.fromEntries(
      els.map((e: any) => [
        e.dataset.agentId,
        {
          needs: e.hasAttribute("data-needs"),
          offline: e.hasAttribute("data-offline"),
          dead: e.hasAttribute("data-dead"),
          state: e.querySelector(".al-state")?.textContent?.trim(),
          dot: e.querySelector(".al-dot")?.getAttribute("aria-label"),
          quick: !!e.querySelector(".al-quick"),
        },
      ]),
    ),
  );
  const f = (id: string) => facts[id] || {};
  expect.soft(f("s8").needs, "awaiting-answer needs you").toBe(true);
  expect.soft(f("s4").needs, "paused needs you").toBe(true);
  expect.soft(f("s11").needs, "blocked needs you").toBe(true);
  expect.soft(f("s12").needs, "errored needs you").toBe(true);
  expect
    .soft(`${f("s3").state} ${f("s3").dot}`, "idle Codex parent with subagents reads Working")
    .toMatch(/Working/);
  expect
    .soft(`${f("s13").state} ${f("s13").dot}`, "stalled parent with subagents reads Working")
    .toMatch(/Working/);
  expect.soft(f("s14").offline, "offline machine").toBe(true);
  expect.soft(f("s14").quick, "offline has no Quick view").toBe(false);
  expect.soft(`${f("s10").state} ${f("s10").dot}`, "crashed").toMatch(/Crash/i);
  expect.soft(f("s7").dead, "completed reads ended").toBe(true);
  await page.waitForTimeout(1500);
  const ticks = await stopStream(page);
  await report(info, "states", { facts, ticks });
});

test("QA keyboard and focus under stream", async ({ mountPane }, info) => {
  const { pane } = await mount(mountPane, "teams");
  const { frame, page } = pane;
  const notes: Record<string, unknown> = {};
  await main(pane, "r0").focus();
  const steps: unknown[] = [];
  for (let i = 0; i < 4; i++) {
    await page.keyboard.press("ArrowDown");
    await page.waitForTimeout(150);
    steps.push(
      await frame.evaluate(() => [
        document.activeElement?.closest(".al-item")?.getAttribute("data-row-id"),
        document.activeElement?.className,
      ]),
    );
  }
  notes.steps = steps;
  notes.afterArrows = await frame.evaluate(() => document.activeElement?.closest(".al-row")?.id);
  await page.waitForTimeout(1000);
  notes.afterWait = await frame.evaluate(() => document.activeElement?.closest(".al-row")?.id);
  expect.soft(notes.afterWait, "arrow-browse focus survives deltas").toBe(notes.afterArrows);
  expect.soft(notes.afterArrows, "arrows moved").not.toBe("agent-row:r0");
  await command(pane, "next-needs-you");
  await page.waitForTimeout(400);
  notes.next = await frame.evaluate(() =>
    document.activeElement?.closest(".al-row")?.hasAttribute("data-needs"),
  );
  expect.soft(notes.next, "next-needs-you lands on needs row").toBe(true);
  await command(pane, "previous-needs-you");
  await page.waitForTimeout(400);
  notes.prev = await frame.evaluate(() =>
    document.activeElement?.closest(".al-row")?.hasAttribute("data-needs"),
  );
  expect.soft(notes.prev).toBe(true);
  await command(pane, "reveal-current");
  await page.waitForTimeout(400);
  notes.reveal = await frame.evaluate(() => document.activeElement?.closest(".al-row")?.id);
  expect.soft(notes.reveal, "reveal-current focuses selected s2 (r2)").toBe("agent-row:r2");
  await command(pane, "focus-search");
  await expect.soft(frame.locator("#al-search")).toBeFocused();
  await main(pane, "r0").focus();
  await page.keyboard.press("Escape");
  await expect.soft(page.locator("#terminal"), "Esc returns to the terminal").toBeFocused();
  const ticks = await stopStream(page);
  const resubscribes = (await pane.verbs()).filter(
    (v) => v.verb === "subscribe" && (v.args as any)?.sinceRev !== undefined,
  ).length;
  await report(info, "keyboard", { ...notes, ticks, harnessShimResubscribes: resubscribes });
});

test("QA filter menu scroll containment in a short dock under stream", async ({
  mountPane,
}, info) => {
  const { pane } = await mount(mountPane, "teams", { height: 300 });
  const { frame, page } = pane;
  await frame.locator("#list").evaluate((el) => (el.scrollTop = 120));
  const listBefore = await frame.locator("#list").evaluate((el) => el.scrollTop);
  await frame.locator("#al-filter").click();
  const menu = frame.locator("#al-menu");
  await expect.soft(menu).toBeVisible();
  const geometry = await menu.evaluate((m) => ({
    scrollHeight: m.scrollHeight,
    clientHeight: m.clientHeight,
    overflowY: getComputedStyle(m).overflowY,
    bottom: m.getBoundingClientRect().bottom,
    viewport: innerHeight,
  }));
  await menu.hover();
  for (let i = 0; i < 6; i++) {
    await page.mouse.wheel(0, 300);
    await page.waitForTimeout(150);
  }
  const menuTop = await frame.evaluate(() => document.getElementById("al-menu")?.scrollTop ?? -1);
  const listAfter = await frame.locator("#list").evaluate((el) => el.scrollTop);
  const docScroll = await frame.evaluate(() => document.scrollingElement?.scrollTop ?? 0);
  expect
    .soft(
      geometry.bottom <= geometry.viewport + 1,
      `menu fits the short dock: ${JSON.stringify(geometry)}`,
    )
    .toBe(true);
  expect.soft(listAfter, "wheel inside the filter menu does not scroll the list").toBe(listBefore);
  expect.soft(docScroll, "wheel does not scroll the pane document").toBe(0);
  if (geometry.scrollHeight > geometry.clientHeight)
    expect.soft(menuTop, "menu itself scrolls").toBeGreaterThan(0);
  await page.waitForTimeout(800);
  const menuTopAfterDeltas = await frame.evaluate(
    () => document.getElementById("al-menu")?.scrollTop ?? -1,
  );
  expect.soft(menuTopAfterDeltas, "menu scroll position survives live repaints").toBe(menuTop);
  const ticks = await stopStream(page);
  await report(info, "menu-scroll", {
    geometry,
    listBefore,
    listAfter,
    menuTop,
    menuTopAfterDeltas,
    docScroll,
    ticks,
  });
});

test("QA filter menu internals: teams checklist, Show and Arrange radios, trigger click-close", async ({
  mountPane,
  browserName,
}, info) => {
  const { pane } = await mount(mountPane, "teams");
  const { frame, page } = pane;
  const notes: Record<string, unknown> = {};
  const filter = frame.locator("#al-filter");
  const menu = frame.locator("#al-menu");
  const inMenu = () =>
    frame.evaluate(() => !!document.getElementById("al-menu")?.contains(document.activeElement));
  await filter.click();
  await expect.soft(menu).toBeVisible();
  // Teams: hide core with the mouse, under deltas; the menu stays and keeps focus.
  const core = menu.locator('[data-choice="team:codesense:core"]');
  await core.click();
  await page.waitForTimeout(800);
  notes.coreChecked = await core.isChecked().catch(() => null);
  notes.menuAfterTeam = await menu.isVisible();
  notes.focusAfterTeam = await inMenu();
  notes.coreRowsShown = await frame.locator(".al-team-head").filter({ hasText: "core" }).count();
  expect.soft(notes.coreChecked, "unchecking a team").toBe(false);
  expect.soft(notes.menuAfterTeam, "menu stays open after a team toggle").toBe(true);
  // macOS WebKit does not focus a checkbox on a mouse click (accepted parity #58).
  if (browserName === "chromium")
    expect.soft(notes.focusAfterTeam, "focus stays in the menu after a team toggle").toBe(true);
  expect.soft(notes.coreRowsShown, "hidden team leaves the list").toBe(0);
  notes.lastTeamDisabled = await menu
    .locator('[data-choice="team:codesense:riverside"]')
    .isDisabled();
  expect.soft(notes.lastTeamDisabled, "the last shown team can't be hidden").toBe(true);
  await menu.locator('[data-choice="team:codesense:core"]').click();
  await page.waitForTimeout(400);
  // Show radios: arrow keys move and wrap, under deltas.
  const showAll = menu.locator('[data-choice="show:all"]');
  await showAll.focus();
  await page.keyboard.press("ArrowDown");
  await page.waitForTimeout(500);
  notes.showAfterDown = await frame.evaluate(
    () => (document.activeElement as HTMLElement)?.dataset.choice,
  );
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowUp");
  await page.waitForTimeout(500);
  notes.showAfterWrap = await frame.evaluate(
    () => (document.activeElement as HTMLElement)?.dataset.choice,
  );
  expect.soft(notes.showAfterDown, "ArrowDown moves the Show radio").toMatch(/^show:(?!all)/);
  expect.soft(notes.showAfterWrap, "ArrowUp wraps the Show radio").toMatch(/^show:/);
  await menu.locator('[data-choice="show:all"]').click();
  // Arrange radios: mouse choice changes the arrangement and the select.
  await menu.locator('[data-choice="arrange:manager"]').click();
  await page.waitForTimeout(600);
  notes.arrangeSelect = await frame.locator("#al-arrange").inputValue();
  notes.menuAfterArrange = await menu.isVisible();
  expect.soft(notes.arrangeSelect, "Arrange radio switches the view").toBe("manager");
  if (!(await menu.isVisible())) await filter.click();
  await menu.locator('[data-choice="arrange:teams"]').click();
  await page.waitForTimeout(400);
  if (!(await menu.isVisible())) await filter.click();
  // A real mouse click on the open trigger closes it and leaves focus on the trigger.
  const fb = (await filter.boundingBox())!;
  await page.mouse.move(fb.x + fb.width / 2, fb.y + fb.height / 2);
  await page.mouse.down();
  await page.waitForTimeout(80);
  notes.openDuringMousedown = await menu.isVisible();
  await page.mouse.up();
  await page.waitForTimeout(400);
  notes.closedAfterClick = !(await menu.isVisible());
  notes.focusOnTrigger = await frame.evaluate(() => document.activeElement?.id === "al-filter");
  notes.expanded = await filter.getAttribute("aria-expanded");
  expect.soft(notes.closedAfterClick, "mouse click on the open trigger closes the menu").toBe(true);
  expect.soft(notes.focusOnTrigger, "click-close returns focus to the trigger").toBe(true);
  expect.soft(notes.expanded).toBe("false");
  // And it opens again on the next click.
  await page.mouse.click(fb.x + fb.width / 2, fb.y + fb.height / 2);
  await page.waitForTimeout(300);
  notes.reopens = await menu.isVisible();
  expect.soft(notes.reopens, "next click reopens").toBe(true);
  const ticks = await stopStream(page);
  await report(info, "menu-internals", { ...notes, ticks });
});
