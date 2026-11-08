// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { expect, test, type Frame } from "../support/quarantine-playwright";
import { viewerScript } from "../support/page-shim";
import { prepareHostPage, startFakeHost, warmHost, type FakeHost } from "./support/fake-host";

const HOST_PAGE = "tests/frame-security/fixtures/lifecycle.html";
let host: FakeHost;
let url: string;
test.beforeAll(async ({ browser }) => {
  test.setTimeout(60_000);
  const shim = viewerScript("LINK_INTERCEPTOR_JS");
  host = await startFakeHost((req, res) => {
    const kitAsset = req.url?.match(/^\/kit\/v1\/(editor\.(js|css))$/);
    if (kitAsset) {
      res.setHeader("content-type", kitAsset[2] === "js" ? "text/javascript" : "text/css");
      res.end(readFileSync(`src-tauri/src/http/routes/dashboard_kit/${kitAsset[1]}`));
      return;
    }
    const id = req.url?.split("/").pop()?.split("?")[0];
    res.setHeader("content-type", "text/html");
    res.end(
      `<!doctype html><script>window.__codesenseProxy={token:${JSON.stringify(id)}};window.log=[];window.silent=false;addEventListener('message',e=>{if(window.silent&&e.data.type==='codesense:ping')e.stopImmediatePropagation()});addEventListener('message',e=>{log.push(e.data);parent.postMessage({fixture:true,id:${JSON.stringify(id)},data:e.data},'*')});${req.url?.includes("nested") ? shim.replaceAll("window.parent", "window.top") : shim}</script><input id="first"><button id="second">Next</button><script>window.navSettled=false;codesense.getNavSelection().then(function(v){window.navRead=v;window.navSettled=true});codesense.ready()</script>`,
    );
  });
  url = host.url(HOST_PAGE);
  await warmHost(browser, host, HOST_PAGE, (warm) =>
    warm.frameLocator("iframe").locator("#first").waitFor({ timeout: 45_000 }),
  );
});
test.afterAll(async () => {
  await host?.close();
});
test.beforeEach(async ({ page }) => {
  page.on("pageerror", (error) => console.log("Fixture page error:", error.message));
  await prepareHostPage(page, host.framePort);
  await page.goto(url);
  await expect(page.locator("iframe")).toHaveCount(1);
  await page.frameLocator("iframe").locator("#first").waitFor();
});
function doc(page: import("@playwright/test").Page): Frame {
  return page.frames().find((f) => f.url().includes("/pane/"))!;
}
async function log(frame: Frame): Promise<string[]> {
  return frame.evaluate(() => (window as any).log.map((m: any) => m.type));
}

test("lifecycle order, visibility, slot context and default focus-in", async ({ page }) => {
  const frame = doc(page);
  await expect.poll(() => log(frame)).toContain("codesense:intent");
  expect(
    (await log(frame)).filter((type) => type !== "codesense:restore-scroll").slice(0, 4),
  ).toEqual([
    "codesense:slot-context",
    "codesense:theme",
    "codesense:visibility",
    "codesense:intent",
  ]);
  const context = await frame.evaluate(
    () => (window as any).log.find((m: any) => m.type === "codesense:slot-context").context,
  );
  expect(context).toMatchObject({
    slotId: "right-pane",
    presentation: "docked",
    narrow: true,
    team: { org: "fixture", team: "dev" },
  });
  await page.evaluate(() => (window as any).control.show(false));
  await expect
    .poll(() =>
      frame.evaluate(
        () =>
          (window as any).log.filter((m: any) => m.type === "codesense:visibility").at(-1).visible,
      ),
    )
    .toBe(false);
  await page.evaluate(() => (window as any).control.show(true));
  await page.locator("#opener").focus();
  await page.locator("iframe").focus();
  await expect(frame.locator("#first")).toBeFocused();
});
test("close retains the document until async flush settles, then tears down", async ({ page }) => {
  const frame = doc(page);
  await frame.evaluate(() =>
    (window as any).codesense.onFlush(() => new Promise((done) => setTimeout(done, 80))),
  );
  await page.evaluate(() => (window as any).control.setOpen(false));
  await expect
    .poll(() => page.evaluate(() => (window as any).events.map((m: any) => m.data.type)))
    .toContain("codesense:flush");
  await expect(page.locator("iframe")).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).events.map((m: any) => m.data.type))).toContain(
    "codesense:teardown",
  );
});
test("a missing ack times out and five quit flushes share one deadline", async ({ page }) => {
  await page.evaluate(() => (window as any).control.setCount(5));
  await expect(page.locator("iframe")).toHaveCount(5);
  for (const frame of page.frames().filter((f) => f.url().includes("/pane/")))
    await frame.waitForFunction(() => !!(window as any).codesense);
  await doc(page).evaluate(() => (window as any).codesense.onFlush(() => new Promise(() => {})));
  const elapsed = await page.evaluate(async () => {
    const start = performance.now();
    await (window as any).control.flushAll(500);
    return performance.now() - start;
  });
  expect(elapsed).toBeLessThan(650);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as any).events.filter((m: any) => m.data.type === "codesense:teardown").length,
      ),
    )
    .toBe(5);
});
test("Esc respects page handling and composition; unhandled Esc exits and restores opener focus", async ({
  page,
}) => {
  await page.locator("#opener").click();
  const frame = doc(page);
  await frame.locator("#first").focus();
  await frame.evaluate(() => {
    (window as any).consume = (e: KeyboardEvent) => {
      if (e.key === "Escape") e.preventDefault();
    };
    addEventListener("keydown", (window as any).consume);
  });
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("overlay")).toHaveAttribute("data-fullscreen", "true");
  await frame.evaluate(() => {
    removeEventListener("keydown", (window as any).consume);
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", isComposing: true, bubbles: true }),
    );
  });
  await page.waitForTimeout(30);
  await expect(page.getByTestId("overlay")).toHaveAttribute("data-fullscreen", "true");
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("overlay")).toHaveAttribute("data-fullscreen", "false");
  await expect(page.locator("#opener")).toBeFocused();
});
test("focusOut returns to the previous host control", async ({ page }) => {
  await page.locator("#terminal").focus();
  await doc(page).locator("#first").click();
  await doc(page).evaluate(() => (window as any).codesense.focusOut({ direction: "backward" }));
  await expect(page.locator("#terminal")).toBeFocused();
});
test("three unanswered pings use the Crashed strip; hidden frames are not pinged", async ({
  page,
}) => {
  await page.clock.install();
  await page.evaluate(() => (window as any).control.show(false));
  await expect
    .poll(() =>
      doc(page).evaluate(
        () =>
          (window as any).log.filter((m: any) => m.type === "codesense:visibility").at(-1)?.visible,
      ),
    )
    .toBe(false);
  await page.clock.fastForward(25000);
  expect((await log(doc(page))).filter((m) => m === "codesense:ping")).toHaveLength(0);
  await page.evaluate(() => (window as any).control.show(true));
  await expect
    .poll(() =>
      doc(page).evaluate(
        () =>
          (window as any).log.filter((m: any) => m.type === "codesense:visibility").at(-1)?.visible,
      ),
    )
    .toBe(true);
  await doc(page).evaluate(() => {
    (window as any).silent = true;
  });
  for (let i = 0; i < 4; i++) {
    await page.clock.fastForward(5000);
    await page.waitForTimeout(20);
  }
  await expect(page.getByTestId("slot-fallback-strip")).toBeVisible();
  await page.getByRole("button", { name: /details/i }).click();
  await expect(page.getByText("frame stopped responding", { exact: false })).toBeVisible();
});

test("focusTerminal and docked Esc use the active workspace terminal", async ({ page }) => {
  const frame = doc(page);
  await frame.locator("#first").click();
  await frame.evaluate(() => (window as any).codesense.focusTerminal());
  await expect(page.locator("#terminal")).toBeFocused();
  await frame.locator("#first").click();
  await page.keyboard.press("Escape");
  await expect(page.locator("#terminal")).toBeFocused();
});

test("a page that never acknowledges close is retired at 300ms and logged once", async ({
  page,
}) => {
  const warnings: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "warning") warnings.push(message.text());
  });
  await doc(page).evaluate(() => (window as any).codesense.onFlush(() => new Promise(() => {})));
  const elapsed = await page.evaluate(async () => {
    const start = performance.now();
    (window as any).control.setOpen(false);
    await new Promise<void>((done) => {
      const observer = new MutationObserver(() => {
        if (!document.querySelector("iframe")) {
          observer.disconnect();
          done();
        }
      });
      observer.observe(document.getElementById("root")!, { childList: true, subtree: true });
    });
    return performance.now() - start;
  });
  expect(elapsed).toBeGreaterThanOrEqual(295);
  expect(elapsed).toBeLessThan(450);
  expect(warnings.filter((message) => message.startsWith("frame flush timed out:"))).toHaveLength(
    1,
  );
});
test("focusOut falls back to the active terminal when its previous element leaves", async ({
  page,
}) => {
  await page.locator("#opener").focus();
  await doc(page).locator("#first").click();
  await page.locator("#opener").evaluate((element) => element.remove());
  await doc(page).evaluate(() => (window as any).codesense.focusOut({ direction: "forward" }));
  await expect(page.locator("#terminal")).toBeFocused();
});

test("direct frame-to-frame entry sends focus-in on A to B to A", async ({ page }) => {
  await page.evaluate(() => (window as any).control.setCount(2));
  await expect(page.locator("iframe")).toHaveCount(2);
  await expect
    .poll(() => page.frames().filter((frame) => frame.url().includes("/pane/")).length)
    .toBe(2);
  const children = page.frames().filter((frame) => frame.url().includes("/pane/"));
  for (const frame of children) {
    await frame.waitForFunction(() => !!(window as any).codesense);
    await frame.evaluate(() => {
      (window as any).entries = 0;
      (window as any).codesense.onFocusIn(() => {
        (window as any).entries++;
      });
    });
  }
  await page.locator("#terminal").focus();
  await children[0].locator("#first").click();
  await expect.poll(() => children[0].evaluate(() => (window as any).entries)).toBe(1);
  await children[1].locator("#first").click();
  await expect.poll(() => children[1].evaluate(() => (window as any).entries)).toBe(1);
  await children[0].locator("#first").click();
  await expect.poll(() => children[0].evaluate(() => (window as any).entries)).toBe(2);
});
test("focus-in skips controls in a closed dialog and other hidden or disabled controls", async ({
  page,
}) => {
  const frame = doc(page);
  await frame.evaluate(() =>
    document.body.insertAdjacentHTML(
      "afterbegin",
      '<dialog><button>Hidden dialog control</button></dialog><button style="display:none">Hidden</button><button style="visibility:hidden">Invisible</button><fieldset disabled><button>Disabled</button></fieldset>',
    ),
  );
  await page.locator("#terminal").focus();
  await page.locator("iframe").focus();
  await expect(frame.locator("#first")).toBeFocused();
});

test("reentry finishes the outgoing save before reminting an interactive frame", async ({
  page,
}) => {
  await page.evaluate(() => {
    (window as any).saveFinished = false;
    addEventListener("message", (event) => {
      if (event.data.saveFinished) (window as any).saveFinished = true;
    });
  });
  await doc(page).evaluate(() =>
    (window as any).codesense.onFlush(async () => {
      await new Promise((done) => setTimeout(done, 250));
      parent.postMessage({ saveFinished: true }, "*");
    }),
  );
  await page.evaluate(() => {
    (window as any).oldFrame = document.querySelector("iframe");
    (window as any).control.setOpen(false);
  });
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as any).events.some((event: any) => event.data.type === "codesense:flush"),
      ),
    )
    .toBe(true);
  await page.evaluate(() => (window as any).control.setOpen(true));
  await expect.poll(() => page.evaluate(() => (window as any).saveFinished)).toBe(true);
  await expect
    .poll(() => page.locator("iframe").evaluate((element) => element !== (window as any).oldFrame))
    .toBe(true);
  await expect(page.locator("iframe")).toBeVisible();
  expect(await page.locator("iframe").evaluate((element) => element.inert)).toBe(false);
  await doc(page).locator("#first").fill("returned");
  await page.waitForTimeout(350);
  await expect(doc(page).locator("#first")).toHaveValue("returned");
});
test("cold-start getNavSelection waits for the host's initial query", async ({ page }) => {
  await page.goto(`${url}?cold-nav`);
  await page.frameLocator("iframe").locator("#first").waitFor();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as any).calls.some(([command]: [string]) => command === "query_context"),
      ),
    )
    .toBe(true);
  expect(await doc(page).evaluate(() => (window as any).navSettled)).toBe(false);
  await page.evaluate(() => (window as any).releaseNav());
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).navRead))
    .toMatchObject({ org: "fixture", team: "dev", file: { path: "/fixture/cold.md" } });
});

test("returning to a page evicted by the 16-frame cap creates a visible focusable iframe", async ({
  page,
}) => {
  await page.goto(`${url}?budget`);
  await page.frameLocator("iframe").locator("#first").waitFor();
  await page.evaluate(() => {
    (window as any).oldFrame = document.querySelector("iframe");
    (window as any).control.parkFirst(true);
    (window as any).control.setCount(17);
  });
  await expect(page.locator("iframe")).toHaveCount(17);
  const first = page.locator("iframe").first();
  await expect.poll(() => first.getAttribute("src")).toBeNull();
  await page.evaluate(() => (window as any).control.parkFirst(false));
  await expect.poll(() => first.getAttribute("src")).toContain("/pane/");
  expect(await first.evaluate((element) => element !== (window as any).oldFrame)).toBe(true);
  expect(await first.evaluate((element) => element.inert)).toBe(false);
  await expect(first).toBeVisible();
  await page.frameLocator("iframe").first().locator("#first").fill("back after eviction");
  await expect(page.frameLocator("iframe").first().locator("#first")).toHaveValue(
    "back after eviction",
  );
});

test("sibling focusOut returns to the previous frame", async ({ page }) => {
  await page.evaluate(() => (window as any).control.setCount(2));
  await expect(page.locator("iframe")).toHaveCount(2);
  await expect.poll(() => page.frames().filter((f) => f.url().includes("/pane/")).length).toBe(2);
  const children = page.frames().filter((f) => f.url().includes("/pane/"));
  for (const child of children) await child.waitForFunction(() => !!(window as any).codesense);
  await page.locator("#terminal").focus();
  await children[0].locator("#first").click();
  await children[1].locator("#first").click();
  await children[1].evaluate(() => (window as any).codesense.focusOut());
  await page.waitForTimeout(30);
  await expect(page.locator("iframe").first()).toBeFocused();
});

test("default focus reaches the first contenteditable editor", async ({ page }) => {
  const frame = doc(page);
  await frame.evaluate(() => {
    document.body.innerHTML =
      '<div id="editor" contenteditable="true">Notes</div><button id="save">Save</button>';
  });
  await page.locator("#terminal").focus();
  await page.locator("iframe").focus();
  await page.waitForTimeout(30);
  await expect(frame.locator("#editor")).toBeFocused();
});

async function menu(page: import("@playwright/test").Page, items: any[]) {
  await doc(page).evaluate((items) => {
    const anchor = document.getElementById("second")!;
    (window as any).menuResult = undefined;
    (window as any).menuError = undefined;
    void (window as any).codesense
      .showMenu({
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        row: { kind: "agent", sessionId: "fixture-session" },
        items,
      })
      .then(
        (result: any) => {
          (window as any).menuResult = result;
        },
        (error: Error) => {
          (window as any).menuError = error.message;
        },
      );
  }, items);
}
test("menus: dashboard selection escapes a 236px frame and returns focus", async ({ page }) => {
  await menu(page, [{ id: "inspect", label: "Inspectaverylongdashboarditemoutsidetheframe" }]);
  const item = page.getByRole("menuitem", { name: "Inspectaverylongdashboarditemoutsidetheframe" });
  await expect(item).toBeVisible();
  const bounds = (await item.boundingBox())!,
    frame = (await page.locator("iframe").boundingBox())!;
  expect(bounds.x + bounds.width).toBeGreaterThan(frame.x + frame.width);
  expect(bounds.x + bounds.width).toBeLessThanOrEqual(await page.evaluate(() => innerWidth - 8));
  await item.click();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toEqual({ itemId: "inspect", row: { kind: "agent", sessionId: "fixture-session" } });
  await expect(doc(page).locator("#second")).toBeFocused();
});
test("menus: keyboard dismissal returns closed and focus-return", async ({ page }) => {
  await menu(page, [{ id: "inspect", label: "Inspect" }]);
  await expect(page.getByRole("menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toEqual({ closed: true });
  await expect(doc(page).locator("#second")).toBeFocused();
  await page.locator("#terminal").focus();
  await page.locator("iframe").focus();
  await expect(doc(page).locator("#first")).toBeFocused();
});
test("menus: native Stop confirmation cannot be answered by the page and cancellation returns focus", async ({
  page,
}) => {
  await menu(page, [{ id: "builtin:stop", label: "Fake label", icon: "zap" }]);
  const stop = page.getByRole("menuitem", { name: "Stop…" });
  await expect(stop).toHaveAttribute("data-variant", "destructive");
  expect(await stop.locator("svg").count()).toBe(0);
  await stop.click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Cancel", exact: true })).toBeFocused();
  const error = await doc(page).evaluate(async () => {
    try {
      await (window as any).codesense.invoke("verb_answer", { allow: true });
    } catch (error) {
      return (error as Error).message;
    }
  });
  expect(error).toContain("not allowlisted");
  await expect(dialog).toBeVisible();
  expect(
    await page.evaluate(() =>
      (window as any).calls.some(([command]: [string]) => command === "dashboard_kill_by_handle"),
    ),
  ).toBe(false);
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(doc(page).locator("#second")).toBeFocused();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "builtin:stop", builtin: true });
});
test("menus: an ordinary Stop item stops nothing and unknown built-ins reject", async ({
  page,
}) => {
  await menu(page, [{ id: "plain", label: "Stop" }]);
  await page.getByRole("menuitem", { name: "Stop", exact: true }).click();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "plain" });
  expect(
    await page.evaluate(() =>
      (window as any).calls.some(([command]: [string]) => command === "dashboard_kill_by_handle"),
    ),
  ).toBe(false);
  await menu(page, ["builtin:relaunch"]);
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuError))
    .toBe('unknown built-in "builtin:relaunch"');
});
test("menus: dashboard and host disabled built-ins cannot be revived", async ({ page }) => {
  await menu(page, [{ id: "builtin:stop", enabled: false }]);
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toHaveAttribute("data-disabled", "");
  await page.keyboard.press("Escape");
  await page.evaluate(() => (window as any).setSession({ alive: false }));
  await menu(page, [{ id: "builtin:stop", enabled: true }]);
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toHaveAttribute("data-disabled", "");
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toHaveAttribute(
    "title",
    "This session has ended.",
  );
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toHaveCSS("pointer-events", "auto");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menu")).toBeVisible();
  await expect(page.getByRole("alertdialog")).not.toBeVisible();
});
test("menus: a declared agent row opens the native default menu", async ({ page }) => {
  const frame = doc(page);
  await frame.locator("#second").evaluate((element) => {
    element.setAttribute(
      "data-codesense-row",
      JSON.stringify({ kind: "agent", sessionId: "fixture-session" }),
    );
  });
  await frame.locator("#second").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: "Quick view", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Role", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(frame.locator("#second")).toBeFocused();
});
test("menus: quick-send popover dismissal returns focus and unknown kinds reject", async ({
  page,
}) => {
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    void (window as any).codesense
      .showPopover({
        kind: "agent-quick-send",
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        sessionId: "fixture-session",
      })
      .then((value: any) => {
        (window as any).popoverResult = value;
      });
  });
  await expect(page.getByRole("dialog", { name: "Quick view of Fixture" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).popoverResult))
    .toEqual({ closed: true });
  await expect(doc(page).locator("#second")).toBeFocused();
  const error = await doc(page).evaluate(async () => {
    try {
      await (window as any).codesense.showPopover({
        kind: "unknown",
        anchorRect: { x: 0, y: 0, width: 0, height: 0 },
      });
    } catch (error) {
      return (error as Error).message;
    }
  });
  expect(error).toBe('unknown popover "unknown"');
});
test("menus: a second request dismisses the first, while arrows and Enter select", async ({
  page,
}) => {
  await menu(page, [{ id: "first", label: "First" }]);
  await expect(page.getByRole("menuitem", { name: "First" })).toBeVisible();
  await doc(page).evaluate(() => {
    (window as any).firstRequest = (window as any).menuResult;
  });
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    void (window as any).codesense
      .showMenu({
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        row: { id: "second" },
        items: [
          { id: "a", label: "Alpha" },
          { id: "b", label: "Beta" },
        ],
      })
      .then((value: any) => {
        (window as any).secondResult = value;
      });
  });
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toEqual({ closed: true });
  await expect(page.getByRole("menuitem", { name: "Alpha" })).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: "Alpha" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: "Beta" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).secondResult))
    .toMatchObject({ itemId: "b", row: { id: "second" } });
});

test("menus: Role uses the host submenu and stores the choice", async ({ page }) => {
  await menu(page, [
    { id: "builtin:set-role", label: "Injected", submenu: [{ id: "fake", label: "Fake role" }] },
  ]);
  await page.getByRole("menuitem", { name: "Role", exact: true }).hover();
  await page.getByRole("menuitemradio", { name: "Worker", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as any).viewer.getState().agentIdentity["fixture:dev"]?.["fixture-agent"]?.role,
      ),
    )
    .toBe("worker");
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "builtin:set-role", builtin: true });
  await expect(doc(page).locator("#second")).toBeFocused();
});
test("menus: Close tab uses the host End dialog and returns focus on Cancel", async ({ page }) => {
  await page.evaluate(() => {
    const viewer = (window as any).viewer;
    viewer.setState((state: any) => ({
      terminalLayout: {
        ...state.terminalLayout,
        byKey: {
          ...state.terminalLayout.byKey,
          "fixture:dev|terminal": {
            tabs: [{ id: "fixture-session", sessionId: "fixture-session", name: "Fixture" }],
            hiddenTabs: [],
            activeTabId: "fixture-session",
          },
        },
      },
    }));
  });
  await menu(page, ["builtin:close-tab"]);
  await page.getByRole("menuitem", { name: "Close tab…" }).click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog.getByRole("heading", { name: "End tab?" })).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(doc(page).locator("#second")).toBeFocused();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "builtin:close-tab", builtin: true });
});
test("menus: Stop runs only after the native confirmation settles", async ({ page }) => {
  await menu(page, ["builtin:stop"]);
  await page.getByRole("menuitem", { name: "Stop…" }).click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toBeVisible();
  await page.waitForTimeout(520);
  await dialog.getByRole("button", { name: "Stop", exact: true }).click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as any).calls.filter(
          ([command]: [string]) => command === "dashboard_kill_by_handle",
        ),
      ),
    )
    .toHaveLength(1);
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "builtin:stop", builtin: true });
});
test("menus: a vanished focus-return anchor falls back to the first control", async ({ page }) => {
  await menu(page, [{ id: "inspect", label: "Inspect" }]);
  await expect(page.getByRole("menuitem", { name: "Inspect" })).toBeVisible();
  await doc(page)
    .locator("#second")
    .evaluate((element) => element.remove());
  await page.keyboard.press("Escape");
  await expect(doc(page).locator("#first")).toBeFocused();
});
test("menus: native submenus flip within the window and show host icons", async ({ page }) => {
  await page.locator("#root").evaluate((element) => {
    element.style.width = "236px";
    element.style.marginLeft = "auto";
  });
  await menu(page, [
    {
      id: "custom",
      label: "Inspect",
      icon: "Search",
      submenu: [{ id: "nested", label: "Nested inspection" }],
    },
    "builtin:stop",
  ]);
  const stop = page.getByRole("menuitem", { name: "Stop…" });
  await expect(stop.locator("svg.lucide-square")).toBeVisible();
  await page.getByRole("menuitem", { name: "Inspect", exact: true }).hover();
  const nested = page.getByRole("menuitem", { name: "Nested inspection" });
  await expect(nested).toBeVisible();
  const box = (await nested.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(8);
  expect(box.x + box.width).toBeLessThanOrEqual(await page.evaluate(() => innerWidth - 8));
  await nested.click();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "nested" });
});

test("menus: an outside click dismisses and checked items use native semantics", async ({
  page,
}) => {
  await menu(page, [
    { id: "checked", label: "Checked option", checked: true },
    { id: "other", label: "Other option" },
  ]);
  await expect(page.getByRole("menuitemcheckbox", { name: "Checked option" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
  await page.locator("#terminal").click({ force: true });
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toEqual({ closed: true });
  await expect(doc(page).locator("#second")).toBeFocused();
});
test("menus: clicking back into the frame dismisses a quick-send popover and keeps the click's focus", async ({
  page,
}) => {
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    void (window as any).codesense
      .showPopover({
        kind: "agent-quick-send",
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        sessionId: "fixture-session",
      })
      .then((value: any) => {
        (window as any).popoverResult = value;
      });
  });
  await expect(page.getByRole("dialog", { name: "Quick view of Fixture" })).toBeVisible();
  await doc(page).locator("#first").click();
  await expect(page.getByRole("dialog", { name: "Quick view of Fixture" })).not.toBeVisible();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).popoverResult))
    .toEqual({ closed: true });
  await expect(doc(page).locator("#first")).toBeFocused();
  expect(await log(doc(page))).not.toContain("codesense:focus-return");
});

test("menus: a selected item can immediately open a popover", async ({ page }) => {
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    const args = {
      anchorRect: anchor.getBoundingClientRect().toJSON(),
      anchorId: "second",
      sessionId: "fixture-session",
    };
    void (window as any).codesense
      .showMenu({ ...args, items: [{ id: "quick", label: "Quick send" }] })
      .then(() => (window as any).codesense.showPopover({ ...args, kind: "agent-quick-send" }))
      .then((value: any) => {
        (window as any).popoverResult = value;
      });
  });
  await page.getByRole("menuitem", { name: "Quick send" }).click();
  const popover = page.getByRole("dialog", { name: "Quick view of Fixture" });
  await expect(popover).toBeVisible();
  await expect(popover.getByRole("button", { name: "Show in CLI" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(popover).not.toBeVisible();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).popoverResult))
    .toEqual({ closed: true });
  await expect(doc(page).locator("#second")).toBeFocused();
});

test("menus: the default menu Quick view opens the native popup", async ({ page }) => {
  const frame = doc(page);
  await frame
    .locator("#second")
    .evaluate((element) =>
      element.setAttribute(
        "data-codesense-row",
        JSON.stringify({ kind: "agent", sessionId: "fixture-session" }),
      ),
    );
  await frame.locator("#second").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Quick view", exact: true }).click();
  const popup = page.getByRole("dialog", { name: "Quick view of Fixture" });
  await expect(popup).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(popup).not.toBeVisible();
  await expect(frame.locator("#second")).toBeFocused();
});
test("menus: Escape closes only the menu in a fullscreen slot", async ({ page }) => {
  await page.locator("#opener").click();
  await expect(page.getByTestId("overlay")).toHaveAttribute("data-fullscreen", "true");
  await menu(page, [{ id: "inspect", label: "Inspect" }]);
  await expect(page.getByRole("menuitem", { name: "Inspect" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).not.toBeVisible();
  await expect(page.getByTestId("overlay")).toHaveAttribute("data-fullscreen", "true");
  await expect(doc(page).locator("#second")).toBeFocused();
});

for (const mode of ["declared", "default"] as const) {
  test(`menus: ${mode} Stop kills a hand-started session after confirmation`, async ({ page }) => {
    await page.evaluate(() => (window as any).setSession({ kind: "hand-started" }));
    const frame = doc(page);
    if (mode === "declared") await menu(page, ["builtin:stop"]);
    else {
      await frame
        .locator("#second")
        .evaluate((el) =>
          el.setAttribute(
            "data-codesense-row",
            JSON.stringify({ kind: "agent", sessionId: "fixture-session" }),
          ),
        );
      await frame.locator("#second").click({ button: "right" });
    }
    await page.getByRole("menuitem", { name: "Stop…" }).click();
    const dialog = page.getByRole("alertdialog");
    await expect(dialog).toContainText("closes this terminal tab");
    const kills = () =>
      page.evaluate(() =>
        (window as any).calls.filter(
          ([command]: [string]) =>
            command === "terminal_close_tab" || command === "dashboard_kill_by_handle",
        ),
      );
    expect(await kills()).toEqual([]);
    await page.waitForTimeout(520);
    await dialog.getByRole("button", { name: "Stop", exact: true }).click();
    await expect
      .poll(kills)
      .toEqual([["terminal_close_tab", { session: "fixture-session", mode: "kill" }]]);
    await expect(frame.locator("#second")).toBeFocused();
    if (mode === "declared")
      await expect
        .poll(() => frame.evaluate(() => (window as any).menuResult))
        .toMatchObject({ itemId: "builtin:stop", builtin: true });
  });
}

test("menus: default Quick view dismisses on a dashboard click and keeps the click's focus", async ({
  page,
}) => {
  const frame = doc(page);
  await frame
    .locator("#second")
    .evaluate((el) =>
      el.setAttribute(
        "data-codesense-row",
        JSON.stringify({ kind: "agent", sessionId: "fixture-session" }),
      ),
    );
  await frame.locator("#second").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Quick view", exact: true }).click();
  const popup = page.getByRole("dialog", { name: "Quick view of Fixture" });
  await expect(popup).toBeVisible();
  await frame.locator("#first").click();
  await expect(popup).not.toBeVisible();
  await page.evaluate(
    () => new Promise((settled) => requestAnimationFrame(() => requestAnimationFrame(settled))),
  );
  await expect(frame.locator("#first")).toBeFocused();
  expect(await log(frame)).not.toContain("codesense:focus-return");
});

test("default menus: row fallbacks, empty space and keyboard anchors never leave a dead click", async ({
  page,
}) => {
  const frame = doc(page);
  await frame.evaluate(
    (markup) => {
      document.body.innerHTML = markup;
    },
    readFileSync("tests/frame-security/fixtures/context-menu-pane.html", "utf8"),
  );
  for (const kind of ["story", "epic", "file", "workspace", "group", "unknown", "agent"]) {
    await frame.locator(`#${kind}`).click({ button: "right" });
    await expect(page.getByRole("menuitem", { name: "Reload pane" })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(frame.locator(`#${kind}`)).toBeFocused();
  }
  await frame.locator("#story").press("Shift+F10");
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  const box = (await menu.boundingBox())!;
  const anchor = (await frame.locator("#story").boundingBox())!;
  expect(box.y).toBeGreaterThanOrEqual(anchor.y);
});

test("text menus: paste is read only on activation and lands in the second unnamed field", async ({
  page,
}) => {
  const frame = doc(page);
  await page.evaluate(() => {
    (window as any).clipboardReads = 0;
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        readText: async () => {
          (window as any).clipboardReads++;
          return "PASTE";
        },
      },
    });
  });
  await frame.evaluate(() => {
    document.body.innerHTML = '<input value="first"><textarea>second</textarea>';
  });
  await frame.locator("textarea").evaluate((el: HTMLTextAreaElement) => {
    el.focus();
    el.setSelectionRange(1, 4);
  });
  await frame.locator("textarea").click({ button: "right", position: { x: 20, y: 8 } });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  expect(await page.evaluate(() => (window as any).clipboardReads)).toBe(0);
  await frame.evaluate(() =>
    window.top!.postMessage({ type: "codesense:text-edit", action: "paste", text: "WRONG" }, "*"),
  );
  expect(await page.evaluate(() => (window as any).clipboardReads)).toBe(0);
  await page.getByRole("menuitem", { name: /^Paste/ }).click();
  await expect(frame.locator("textarea")).toHaveValue("sPASTEnd");
  await expect(frame.locator("input")).toHaveValue("first");
  await expect(frame.locator("textarea")).toBeFocused();
  expect(await page.evaluate(() => (window as any).clipboardReads)).toBe(1);
  await frame.locator("textarea").press("ControlOrMeta+z");
  await expect(frame.locator("textarea")).toHaveValue("second");
  await frame.locator("textarea").evaluate((field: HTMLTextAreaElement) => {
    field.setSelectionRange(1, 4);
    field.addEventListener("contextmenu", (event) => event.preventDefault(), { once: true });
  });
  await frame.locator("textarea").click({ button: "right", position: { x: 20, y: 8 } });
  await expect(page.getByRole("menu")).toHaveCount(0);
  await frame.locator("textarea").press("ControlOrMeta+a");
  await frame.locator("textarea").press("ArrowLeft");
  await frame.locator("textarea").press("Shift+F10");
  await expect(page.getByRole("menuitem", { name: /^Copy/ })).toBeDisabled();
  await page.keyboard.press("Escape");
});

test("text menus: readonly, disabled, password, email and contenteditable", async ({ page }) => {
  const frame = doc(page);
  await frame.evaluate(() => {
    document.body.innerHTML =
      '<input id="readonly" readonly value="read"><input id="disabled" disabled value="disabled"><input id="password" type="password" value="secret"><input id="email" type="email" value="12345"><input id="number" type="number" value="12345"><div id="editable" contenteditable>edit me</div>';
  });
  for (const id of ["readonly", "disabled", "password", "email", "number", "editable"]) {
    await frame.locator(`#${id}`).click({ button: "right", force: true });
    await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
    if (["readonly", "disabled"].includes(id))
      await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeDisabled();
    if (id === "password") {
      await expect(page.getByRole("menuitem", { name: /^Cut/ })).toBeDisabled();
      await expect(page.getByRole("menuitem", { name: /^Copy/ })).toBeDisabled();
    }
    await page.keyboard.press("Escape");
  }
});

test("text menus: email and number paste preserve the browser caret and input path", async ({
  page,
}) => {
  await page.evaluate(() =>
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { readText: async () => "12" },
    }),
  );
  const frame = doc(page);
  await frame.evaluate(() => {
    document.body.innerHTML =
      '<input id="email" type="email" value="12345"><input id="number" type="number" value="12345">';
  });
  for (const id of ["email", "number"]) {
    const field = frame.locator(`#${id}`);
    await field.focus();
    await field.press("ControlOrMeta+a");
    await field.press("ArrowLeft");
    await field.press("ArrowRight");
    await field.press("ArrowRight");
    await field.press("Shift+F10");
    await page.getByRole("menuitem", { name: /^Paste/ }).click();
    await expect(field).toHaveValue("1212345");
    await field.press("ControlOrMeta+z");
    await expect(field).toHaveValue("12345");
    await field.press("ControlOrMeta+a");
    await field.press("Shift+F10");
    await expect(page.getByRole("menuitem", { name: /^Copy/ })).toBeEnabled();
    await page.keyboard.press("Escape");
  }
});

test("default menus close when the frame reloads", async ({ page }) => {
  await doc(page).locator("#first").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await doc(page).evaluate(() => location.reload());
  await expect(page.getByRole("menu")).toHaveCount(0);
});

test("native fields inside a menu trigger get the same text menu", async ({ page }) => {
  await page.evaluate(() => {
    const trigger = document.createElement("div");
    trigger.dataset.slot = "context-menu-trigger";
    trigger.innerHTML = '<input value="native"><textarea>second native</textarea>';
    trigger.addEventListener("contextmenu", () => {
      (window as any).rowMenuOpened = true;
    });
    document.body.append(trigger);
  });
  await page.locator("textarea").last().click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  expect(await page.evaluate(() => (window as any).rowMenuOpened)).toBeUndefined();
  await page.keyboard.press("Escape");
  await expect(page.locator("textarea").last()).toBeFocused();
});

for (const mode of ["group", "new-group"]) {
  test(`native text menu keeps the ${mode} naming dialog mounted`, async ({ page }) => {
    await page.evaluate((mode) => {
      (window as any).mountNativeEditors(mode);
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: { readText: async () => "Pasted group" },
      });
    }, mode);
    await page.getByRole("menuitem", { name: mode === "group" ? "Rename…" : "New group…" }).click();
    const input = page.getByRole("textbox", { name: "Group name" });
    await input.fill("Original");
    await input.press("ControlOrMeta+a");
    await input.click({ button: "right" });
    await expect(input).toBeAttached();
    await page.getByRole("menuitem", { name: /^Paste/ }).click();
    await expect(input).toHaveValue("Pasted group");
    await expect(input).toBeFocused();
    await input.press("Shift+F10");
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menuitem", { name: /^Paste/ })).toHaveCount(0);
    await expect(input).toBeVisible();
    await expect(input).toBeFocused();
    await input.press("Escape");
    await expect(input).toHaveCount(0);
  });
}

for (const mode of ["terminal", "notes", "inline"]) {
  test(`native text menu preserves the ${mode} rename until focus leaves both`, async ({
    page,
  }) => {
    await page.evaluate((mode) => {
      (window as any).mountNativeEditors(mode);
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: { readText: async () => "Pasted name" },
      });
    }, mode);
    if (mode === "notes") {
      await page.getByRole("option", { name: /Original/ }).hover();
      await page.getByRole("button", { name: "More actions for Original" }).click();
      await page.getByRole("button", { name: "Rename", exact: true }).click();
    }
    const input = page.getByRole("textbox", {
      name: mode === "terminal" ? "Tab name" : mode === "notes" ? "Rename Original" : "Inline name",
    });
    await input.press("ControlOrMeta+a");
    await input.press("Shift+F10");
    await expect(input).toBeAttached();
    expect(await page.evaluate(() => (window as any).nativeRenames)).toEqual([]);
    await page.getByRole("menuitem", { name: /^Paste/ }).click();
    await expect(input).toHaveValue("Pasted name");
    await expect(input).toBeFocused();
    expect(await page.evaluate(() => (window as any).nativeRenames)).toEqual([]);
    await input.press("Shift+F10");
    await page.getByRole("button", { name: "Outside editor" }).click();
    await expect(input).toHaveCount(0);
    expect(await page.evaluate(() => (window as any).nativeRenames)).toEqual(["Pasted name"]);
  });
}

for (const mode of ["metadata-chip", "metadata-text", "metadata-status", "field-draft"]) {
  test(`native text menu preserves ${mode} until its pasted value is committed`, async ({
    page,
  }) => {
    await page.evaluate((mode) => {
      (window as any).mountNativeEditors(mode);
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: { readText: async () => "SV-PLG-13" },
      });
    }, mode);
    if (mode === "metadata-chip")
      await page.getByRole("button", { name: "add", exact: true }).click();
    const input = page.getByLabel(
      mode === "metadata-chip"
        ? "Add depends_on"
        : mode === "metadata-text"
          ? "Metadata text"
          : mode === "metadata-status"
            ? "Metadata status"
            : "Main doc",
      { exact: true },
    );
    await input.fill("");
    await input.click({ button: "right" });
    await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
    await expect(input).toBeAttached();
    expect(await page.evaluate(() => (window as any).nativeRenames)).toEqual([]);
    await page.getByRole("menuitem", { name: /^Paste/ }).click();
    await expect(input).toHaveValue("SV-PLG-13");
    await expect(input).toBeFocused();
    expect(await page.evaluate(() => (window as any).nativeRenames)).toEqual([]);
    await page.getByRole("button", { name: "Outside editor" }).click();
    await expect
      .poll(() => page.evaluate(() => (window as any).nativeRenames))
      .toEqual(["SV-PLG-13"]);
  });
}

test("native text menu keeps the custom font input mounted until Paste finishes", async ({
  page,
}) => {
  await page.evaluate(() => {
    (window as any).mountNativeEditors("font");
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { readText: async () => "23" },
    });
  });
  const input = page.getByLabel("Viewer font size in pixels", { exact: true });
  await input.fill("19");
  await input.press("ControlOrMeta+a");
  await input.press("Shift+F10");
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  expect(await page.evaluate(() => (window as any).nativeFontPx())).toBe(0);
  await page.getByRole("menuitem", { name: /^Paste/ }).click();
  await expect(input).toHaveValue("23");
  await expect(input).toBeFocused();
  expect(await page.evaluate(() => (window as any).nativeFontPx())).toBe(0);
  await page.getByRole("button", { name: "Outside editor" }).click();
  await expect.poll(() => page.evaluate(() => (window as any).nativeFontPx())).toBe(23);
});

test("native text menu never pastes into a field removed during a clipboard read", async ({
  page,
}) => {
  await page.evaluate(() => {
    const field = document.createElement("input");
    field.id = "removed-native-field";
    field.value = "Original";
    document.body.prepend(field);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        readText: () =>
          new Promise((resolve) => {
            (window as any).finishPaste = resolve;
          }),
      },
    });
    (window as any).execCalls = 0;
    const exec = document.execCommand.bind(document);
    document.execCommand = (...args) => {
      (window as any).execCalls++;
      return exec(...args);
    };
  });
  await page.locator("#removed-native-field").click({ button: "right" });
  await page.getByRole("menuitem", { name: /^Paste/ }).click();
  await page.evaluate(() => {
    document.getElementById("removed-native-field")!.remove();
    (window as any).finishPaste("Pasted");
  });
  await page.evaluate(() => new Promise(requestAnimationFrame));
  expect(await page.evaluate(() => (window as any).execCalls)).toBe(0);
});

test("hosted text menus measure the child through the parent and return focus", async ({
  page,
}) => {
  await doc(page).evaluate(() => {
    const child = document.createElement("iframe");
    child.id = "nested";
    child.src = location.origin + "/nested";
    child.style.cssText = "position:absolute;left:24px;top:65px;width:180px;height:150px;border:0";
    child.allow = "clipboard-write";
    document.body.append(child);
  });
  await page.evaluate(() => {
    const parent = document.querySelector("iframe")!;
    (window as any).mountViewer({
      filePath: "/fixture/nested",
      teamContentPath: "/fixture",
      hostTeamKey: "fixture:dev",
      parentElement: parent,
      hostedFrame: {
        frameId: "nested",
        token: "nested",
        origin: `http://127.0.0.1:${(window as any).framePort}`,
        documentPath: "/fixture/nested",
        contentRoot: "/fixture",
        grant: {
          scope: "open",
          team: { org: "fixture", team: "dev" },
          principal: "fixture",
          dashboard: "nested",
          slot: "message",
        },
      },
    });
  });
  const nested = page.frameLocator("iframe").frameLocator("#nested");
  await nested.locator("#first").click({ button: "right" });
  const paste = page.getByRole("menuitem", { name: /^Paste/ });
  await expect(paste).toBeVisible();
  const box = (await paste.boundingBox())!;
  const anchor = (await nested.locator("#first").boundingBox())!;
  expect(box.y).toBeGreaterThanOrEqual(anchor.y);
  await page.keyboard.press("Escape");
  await expect(nested.locator("#first")).toBeFocused();
  await expect
    .poll(() => nested.locator("#first").evaluate(() => (window as any).codesense.textMenuOpen))
    .toBe(false);
});

test("served kit editor language input survives the host text menu", async ({ page }) => {
  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { readText: async () => "typescript" },
    });
  });
  const frame = doc(page);
  await frame.evaluate(async () => {
    const style = document.createElement("link");
    style.rel = "stylesheet";
    style.href = "/kit/v1/editor.css";
    document.head.append(style);
    const root = document.createElement("div");
    document.body.append(root);
    const { createEditor } = await import("/kit/v1/editor.js");
    (window as any).kitEditor = createEditor(root, {
      markdown: "```javascript\nconst n = 1;\n```",
    });
  });
  const label = frame.locator(".scratch-code-block-lang-label");
  const input = frame.locator(".scratch-code-block-lang-input");
  await label.click();
  await input.fill("");
  await input.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await expect.poll(() => frame.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(true);
  // Exercise the shipped DOM picker's 150ms blur reset before activating Paste.
  await page.waitForTimeout(220);
  await expect(input).toBeAttached();
  await page.getByRole("menuitem", { name: /^Paste/ }).click();
  await expect(input).toHaveValue("typescript");
  await expect(input).toBeFocused();
  await expect.poll(() => frame.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(false);
  await input.press("Enter");
  await expect(label).toHaveText("typescript");
  expect(await frame.evaluate(() => (window as any).kitEditor.getMarkdown())).toContain(
    "```typescript",
  );

  await label.click();
  await input.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(input).toBeFocused();
  await page.waitForTimeout(220);
  await expect(input).toBeVisible();
  await input.press("Escape");
  await expect(input).toHaveCount(0);
  await expect(label).toHaveText("typescript");

  await label.click();
  await frame.locator("#second").click();
  await expect(input).toHaveCount(0);

  await label.click();
  await page.getByRole("textbox", { name: "terminal", exact: true }).click();
  await expect(input).toHaveCount(0);
});

test("text menus: CodeMirror paste uses the editor input and undo path", async ({ page }) => {
  await page.evaluate(() =>
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { readText: async () => "PASTE" },
    }),
  );
  const frame = doc(page);
  await frame.evaluate((href) => {
    const script = document.createElement("script");
    script.type = "module";
    script.src = href;
    document.body.append(script);
  }, host.url("tests/frame-security/fixtures/context-menu-editor.ts"));
  await frame.locator(".cm-content").waitFor();
  await frame.evaluate(() => {
    const editor = (window as any).editor;
    editor.dispatch({ selection: { anchor: 0, head: 6 } });
    editor.focus();
  });
  await frame.locator(".cm-content").click({ button: "right", position: { x: 20, y: 10 } });
  await page.getByRole("menuitem", { name: /^Paste/ }).click();
  await expect
    .poll(() => frame.evaluate(() => (window as any).editor.state.doc.toString()))
    .toBe("PASTE text");
  await frame.locator(".cm-content").press("ControlOrMeta+z");
  await expect
    .poll(() => frame.evaluate(() => (window as any).editor.state.doc.toString()))
    .toBe("editor text");
});

test("baseline frames suppress row menus and still show the text menu", async ({ page }) => {
  await page.evaluate(() =>
    (window as any).mountViewer({
      filePath: "/fixture/before.html",
      teamContentPath: "/fixture",
      baselineSource: { orgRoot: "/fixture", relPath: "before.html" },
    }),
  );
  await expect
    .poll(() => page.frames().some((f) => f.url().includes("/viewer-baseline/")))
    .toBe(true);
  const before = page.frames().find((f) => f.url().includes("/viewer-baseline/"))!;
  await before.locator("#first").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Paste/ })).toBeVisible();
  await expect.poll(() => before.evaluate(() => (window as any).codesense.textMenuOpen)).toBe(true);
  await page.keyboard.press("Escape");
  await expect
    .poll(() => before.evaluate(() => (window as any).codesense.textMenuOpen))
    .toBe(false);
  await before.locator("#second").click({ button: "right" });
  await expect(page.getByRole("menu")).toHaveCount(0);
});
