// SPDX-License-Identifier: MIT
import { expect, test, type MountedPane } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

function fixtures() {
  const f = agentListFixtures(30, "teams");
  f.width = 236;
  const state = f.topics!.agentList.find((row) => row.rowType === "state")!;
  state.pane = {
    ...(state.pane as object),
    openTeams: ["fixture:dev"],
    teamOrder: ["fixture:dev"],
    rails: { "fixture:dev": [] },
    railKeys: { "fixture:dev": {} },
    pinnedRefs: [],
  };
  (f.topics!["ui:dock"][0] as any).folds = Object.fromEntries(
    [0, 1, 2].map((i) => [`#ws-fold:fixture:dev|ws${i}`, true]),
  );
  return f;
}
const card = (pane: MountedPane, id = "ws0") =>
  pane.frame.locator(`[data-row-id="workspace:fixture:dev|${id}"] .al-ws-head`);
async function cardBounds(pane: MountedPane, id = "ws0") {
  const target = card(pane, id);
  await expect(target).toBeVisible();
  let previous: string | undefined,
    rect = { x: 0, y: 0, width: 0, height: 0 };
  await expect
    .poll(async () => {
      rect = await target.evaluate((el) => {
        const r = el.getBoundingClientRect();
        return { x: r.x, y: r.y, width: r.width, height: r.height };
      });
      const current = JSON.stringify(rect),
        stable = current === previous && rect.width > 0 && rect.height > 0;
      previous = current;
      return stable;
    })
    .toBe(true);
  const frame = (await (await pane.frame.frameElement()).boundingBox())!;
  return { ...rect, x: frame.x + rect.x, y: frame.y + rect.y };
}
async function startDrag(pane: MountedPane) {
  const b = await cardBounds(pane);
  await pane.page.mouse.move(b.x + 50, b.y + 15);
  await pane.page.mouse.down();
  await pane.page.mouse.move(b.x + 50, b.y + 23);
}
async function movable(pane: MountedPane) {
  await pane.page.evaluate(() =>
    (window as any).viewer.setState({
      workspaces: {
        "fixture:dev": [0, 1, 2].map((i) => ({
          id: `ws${i}`,
          kind: "generic",
          status: "ready",
          cwd: `/ws${i}`,
        })),
      },
      trayBaseOrderByTeam: { "fixture:dev": ["ws0", "ws1", "ws2"] },
    }),
  );
}

async function publishOrder(pane: MountedPane, f: ReturnType<typeof fixtures>, ids: string[]) {
  for (const ref of f.topics!.agentList.filter((row) => row.sessionRef))
    await pane.upsert("agentList", {
      ...ref,
      workspaceRank: ids.indexOf(String(ref.workspaceRef)),
    });
}

test("card drag uses native after anchor and waits for hub order; keyboard uses before", async ({
  mountPane,
}) => {
  const f = fixtures(),
    pane = await mountPane(AGENT_LIST_PANE, f);
  await movable(pane);
  await expect(pane.frame.locator(".al-ws-grip")).toHaveCount(3);
  await expect(card(pane)).toHaveAttribute("aria-keyshortcuts", "Alt+ArrowUp Alt+ArrowDown");
  await startDrag(pane);
  await expect(pane.frame.locator(".al-ws-drag-preview")).toBeVisible();
  const target = await cardBounds(pane, "ws2");
  await pane.page.mouse.move(target.x + 50, target.y + target.height - 2);
  await expect(pane.frame.locator(".al-drop-line")).toBeVisible();
  await pane.page.mouse.up();
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({
      verb: "moveWorkspace",
      args: { teamKey: "fixture:dev", workspaceId: "ws0", after: "ws2" },
    });
  await expect(pane.frame.locator("#al-move-notice")).toHaveText("Moved Work 0 below Work 2");
  await expect(pane.frame.locator(".al-ws-name")).toHaveText(["Work 0", "Work 1", "Work 2"]);
  await publishOrder(pane, f, ["ws1", "ws2", "ws0"]);
  await expect(pane.frame.locator(".al-ws-name")).toHaveText(["Work 1", "Work 2", "Work 0"]);
  await card(pane).focus();
  await pane.page.keyboard.press("Alt+ArrowUp");
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({
      verb: "moveWorkspace",
      args: { teamKey: "fixture:dev", workspaceId: "ws0", before: "ws2" },
    });
  await expect(card(pane)).toBeFocused();
  await publishOrder(pane, f, ["ws1", "ws0", "ws2"]);
  await expect(pane.frame.locator(".al-ws-name")).toHaveText(["Work 1", "Work 0", "Work 2"]);
  await expect(card(pane)).toBeFocused();
});

test("refused presses stay clicks and explain filters, no pill and nowhere", async ({
  mountPane,
}) => {
  const pane = await mountPane(AGENT_LIST_PANE, fixtures());
  await pane.writeUi({ search: { query: "Agent", status: "all" } });
  await expect(pane.frame.locator(".al-ws-grip")).toHaveCount(0);
  await startDrag(pane);
  await expect(pane.frame.locator("#al-move-notice")).toContainText(
    "Cards can't move while a search or filter is on.",
  );
  await pane.page.mouse.up();
  await expect(pane.frame.locator(".al-ws-drag-preview")).toHaveCount(0);
  expect((await pane.verbs()).filter((v) => v.verb === "moveWorkspace")).toEqual([]);
  await pane.writeUi({ search: { query: "", status: "all" } });
  const state = fixtures().topics!.agentList.find((r) => r.rowType === "state")!;
  await pane.upsert("agentList", {
    ...state,
    pane: { ...(state.pane as object), workspaceOrder: { "fixture:dev": ["ws1", "ws2"] } },
  });
  await card(pane).focus();
  await pane.page.keyboard.press("Alt+ArrowDown");
  await expect(pane.frame.locator("#al-move-notice")).toContainText(
    "This card has no pill in the tray",
  );
  await pane.upsert("agentList", {
    ...state,
    pane: { ...(state.pane as object), workspaceOrder: { "fixture:dev": ["ws0"] } },
  });
  await startDrag(pane);
  await expect(pane.frame.locator("#al-move-notice")).toContainText(
    "This card has no other card in its team",
  );
  await pane.page.mouse.up();
});

test("a host-refused drop snaps back and announces failure", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, fixtures());
  await startDrag(pane);
  const target = await cardBounds(pane, "ws2");
  await pane.page.mouse.move(target.x + 50, target.y + target.height - 2);
  await pane.page.mouse.up();
  await expect(pane.frame.locator("#al-move-notice")).toContainText("This card couldn't move");
  await expect(pane.frame.locator(".al-ws-name")).toHaveText(["Work 0", "Work 1", "Work 2"]);
  await expect(pane.frame.locator(".al-ws-drag-preview")).toHaveCount(0);
});

for (const cancel of ["Escape", "pointercancel", "lostpointercapture", "blur", "teardown"])
  test(`${cancel} clears capture and drag state without sending a move`, async ({ mountPane }) => {
    const pane = await mountPane(AGENT_LIST_PANE, fixtures());
    await startDrag(pane);
    await expect(pane.frame.locator("html")).toHaveAttribute("data-al-card-drag", "");
    if (cancel === "Escape") {
      await pane.page.keyboard.press("Escape");
      await expect(pane.page.locator("#terminal")).not.toBeFocused();
      expect((await pane.verbs()).filter((v) => v.verb === "focusTerminal")).toEqual([]);
    } else if (cancel === "teardown") await pane.close();
    else
      await pane.frame.evaluate((kind) => {
        if (kind === "lostpointercapture") {
          const el = document.getElementById("list")!;
          el.addEventListener(
            "pointermove",
            (e) => el.releasePointerCapture((e as PointerEvent).pointerId),
            { once: true },
          );
        } else window.dispatchEvent(new Event(kind));
      }, cancel);
    if (cancel === "lostpointercapture") {
      await pane.page.mouse.move(100, 260);
      await pane.page.mouse.move(100, 261);
    }
    if (cancel !== "teardown") {
      await expect(pane.frame.locator(".al-ws-drag-preview")).toHaveCount(0);
      await expect(pane.frame.locator("html")).not.toHaveAttribute("data-al-card-drag");
    }
    await pane.page.mouse.up();
    expect((await pane.verbs()).filter((v) => v.verb === "moveWorkspace")).toEqual([]);
  });

test("Option+Down moves one place; an unmovable short press still folds", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, fixtures());
  await movable(pane);
  await card(pane).focus();
  await pane.page.keyboard.press("Alt+ArrowDown");
  await expect
    .poll(() => pane.verbs())
    .toContainEqual({
      verb: "moveWorkspace",
      args: { teamKey: "fixture:dev", workspaceId: "ws0", after: "ws1" },
    });
  const state = fixtures().topics!.agentList.find((r) => r.rowType === "state")!;
  await pane.upsert("agentList", {
    ...state,
    pane: { ...(state.pane as object), workspaceOrder: { "fixture:dev": [] } },
  });
  await expect(pane.frame.locator(".al-ws-grip")).toHaveCount(0);
  await card(pane).click();
  await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
});

for (const reducedMotion of [false, true])
  test(`hub card moves use FLIP with reduced motion ${reducedMotion}`, async ({ mountPane }) => {
    const f = fixtures(),
      pane = await mountPane(AGENT_LIST_PANE, f);
    await pane.page.emulateMedia({ reducedMotion: reducedMotion ? "reduce" : "no-preference" });
    await pane.frame.evaluate(() => {
      const animate = Element.prototype.animate;
      (window as any).cardAnimations = 0;
      Element.prototype.animate = function (...args) {
        if (this.matches("[data-row-id]")) (window as any).cardAnimations++;
        return animate.apply(this, args);
      };
    });
    await pane.upsert("agentList", { ...f.topics!.agentList[0], workspaceRank: 5 });
    await expect(pane.frame.locator(".al-ws-name")).toHaveText(["Work 1", "Work 2", "Work 0"]);
    const count = await pane.frame.evaluate(() => (window as any).cardAnimations);
    if (reducedMotion) expect(count).toBe(0);
    else expect(count).toBeGreaterThan(0);
  });

for (const leave of ["Escape", "Tab", "click"])
  test(`${leave} cancels keyboard reorder focus intent before the result and order arrive`, async ({
    mountPane,
    browserName,
  }) => {
    const f = fixtures(),
      pane = await mountPane(AGENT_LIST_PANE, f);
    await movable(pane);
    await pane.page.evaluate(() => {
      const api = window as any,
        invoke = api.__TAURI_INTERNALS__.invoke;
      const gate = new Promise<void>((resolve) => {
        api.releaseMove = resolve;
      });
      api.__TAURI_INTERNALS__.invoke = async (command: string, args: any) => {
        if (command === "verb_dispatch" && args.verb === "moveWorkspace") {
          api.moveWaiting = true;
          await gate;
        }
        return invoke(command, args);
      };
    });
    await card(pane).focus();
    await pane.page.keyboard.press("Alt+ArrowDown");
    await expect.poll(() => pane.page.evaluate(() => (window as any).moveWaiting)).toBe(true);
    if (leave === "click") await pane.frame.locator("#al-search").click();
    else
      await pane.page.keyboard.press(
        leave === "Tab" && browserName === "webkit" ? "Alt+Tab" : leave,
      );
    // The workspace Launch control follows its head in the tab order.
    if (leave === "Tab")
      await pane.page.keyboard.press(browserName === "webkit" ? "Alt+Tab" : "Tab");
    const destination =
      leave === "Escape"
        ? pane.page.locator("#terminal")
        : leave === "Tab"
          ? card(pane, "ws1")
          : pane.frame.locator("#al-search");
    await expect(destination).toBeFocused();
    await pane.page.evaluate(() => (window as any).releaseMove());
    await expect(pane.frame.locator("#al-move-notice")).toHaveText("Moved Work 0 below Work 1");
    await expect(destination).toBeFocused();
    await publishOrder(pane, f, ["ws1", "ws0", "ws2"]);
    await expect(pane.frame.locator(".al-ws-name")).toHaveText(["Work 1", "Work 0", "Work 2"]);
    await expect(destination).toBeFocused();
  });
