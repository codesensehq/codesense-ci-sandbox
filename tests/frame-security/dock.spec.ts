// SPDX-License-Identifier: MIT
import { expect, test, type Frame, type Page } from "../support/quarantine-playwright";
import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { createServer as createViteServer, type ViteDevServer } from "vite";
import tailwindcss from "@tailwindcss/vite";

// The dock slot host with fixture dashboards (test only, never shipped): `dock-ok` lists agent
// names from first paint and `subscribe('agentList')`, calls ready(), declares one row menu and
// leaves Esc unhandled; `dock-never` never calls ready(); `dock-throw` throws on load.
let frames: Server;
let vite: ViteDevServer;
let base: string;
let framePort: number;
const loads = new Map<string, number>();
const FIRST_PAINT = {
  topic: "agentList",
  viewKey: "all",
  rev: 1,
  snapshot: [{ id: "fixture-agent", sessionRef: "fixture-session" }],
};
function fixtureBody(name: string): string {
  // `dock-flaky` throws on its first load and starts on the next.
  if (name === "dock-throw.html" || (name === "dock-flaky.html" && loads.get(name) === 1))
    return `<script>throw new Error("fixture failed to start")</script>`;
  const ok = name === "dock-ok.html" || name === "dock-flaky.html" || name === "agent-list.html";
  return `<meta name="codesense:first-paint" content="agentList">
<ul id="rows"></ul>
<button id="row" data-codesense-row='{"kind":"agent","sessionId":"fixture-session"}'>fixture-agent</button>
<button id="second">More</button>
<script>
window.rendered=[];window.received=[];addEventListener('message',function(e){if(e.data&&e.data.type)received.push(e.data.type)});
window.commands=[];codesense.onCommand(function(c){window.commands.push(c)});
function render(rows,from){document.getElementById('rows').innerHTML=rows.map(function(r){return '<li>'+r.id+'</li>'}).join('');window.rendered.push(from)}
var fp=codesense.firstPaint;if(fp)render(fp.snapshot,'firstPaint');
function sub(){codesense.invoke('subscribe',{topic:'agentList'}).then(function(s){render(s.snapshot,'subscribe')})}
${ok ? "codesense.ready();sub();" : ""}
addEventListener('message',function(e){var d=e.data;if(d&&d.type==='codesense:resync'){(window.resyncs=window.resyncs||[]).push(d.topic);if(d.topic==='agentList')sub()}});
codesense.onSlotContext(function(c){window.ctx=c});
codesense.onFlush(function(){return new Promise(function(r){setTimeout(function(){parent.postMessage({fixture:'saved'},'*');r()},100)})});
codesense.onTeardown(function(){parent.postMessage({fixture:'teardown'},'*')});
</script>`;
}
test.beforeAll(async ({ browser }) => {
  test.setTimeout(60_000);
  const source = await readFile(resolve("src-tauri/src/http/routes/viewer.rs"), "utf8");
  const shim = source.match(/const LINK_INTERCEPTOR_JS: &str = r#"([\s\S]*?)"#;/)![1];
  frames = createServer((req, res) => {
    const id = req.url?.split("/").pop()?.split("?")[0] ?? "";
    // Document loads only; the host's own existence probe is a fetch.
    if (req.headers["sec-fetch-dest"] === "iframe") loads.set(id, (loads.get(id) ?? 0) + 1);
    // `dock-hang` never finishes loading.
    if (id === "dock-hang.html" && req.headers["sec-fetch-dest"] === "iframe") return;
    res.setHeader("content-type", "text/html");
    res.setHeader("access-control-allow-origin", "*");
    // As the Rust route does: the token, then the first paint, then the shim, then the page.
    res.end(
      `<!doctype html><script>window.__codesenseProxy={token:${JSON.stringify(id)}};window.__codesenseFirstPaint=${JSON.stringify(FIRST_PAINT)};${shim}</script>${fixtureBody(id)}`,
    );
  });
  await new Promise<void>((done) => frames.listen(0, "127.0.0.1", done));
  framePort = (frames.address() as { port: number }).port;
  vite = await createViteServer({
    configFile: false,
    plugins: [tailwindcss()],
    root: process.cwd(),
    server: { port: 0, host: "localhost" },
    resolve: { alias: { "@": resolve("src") } },
    esbuild: { jsx: "automatic" },
    logLevel: "error",
  });
  await vite.listen();
  base = `http://localhost:${(vite.httpServer!.address() as { port: number }).port}/tests/frame-security/fixtures/dock.html`;
  const warm = await browser.newPage();
  try {
    await warm.addInitScript((port) => {
      (window as any).framePort = port;
    }, framePort);
    await warm.goto(base, { waitUntil: "domcontentloaded" });
    await warm.frameLocator("iframe").locator("#row").waitFor({ timeout: 45_000 });
  } finally {
    await warm.close();
  }
});
test.afterAll(async () => {
  await vite?.close();
  await new Promise<void>((done) => (frames ? frames.close(() => done()) : done()));
});
async function open(page: Page, query = "page=dock-ok") {
  page.on("pageerror", (error) => console.log("Fixture page error:", error.message));
  await page.addInitScript((port) => {
    (window as any).framePort = port;
  }, framePort);
  loads.clear();
  await page.bringToFront();
  await page.goto(`${base}?${query}`, { waitUntil: "domcontentloaded" });
}
async function ready(page: Page) {
  await page.frameLocator("iframe").locator("#row").waitFor();
}
function doc(page: Page): Frame {
  return page.frames().find((f) => f !== page.mainFrame() && f.url().includes("dock-"))!;
}

test("dock commands reveal the slot, wait for page readiness, and stay live across team rebind", async ({
  page,
}) => {
  await open(page, "page=dock-await-ready");
  await ready(page);
  await page.evaluate(() => {
    (window as any).foldDock();
    (window as any).runDockCommand({ stage: "queued" });
  });
  await expect(page.getByTestId("agents-dock")).toBeVisible();
  expect(await doc(page).evaluate(() => (window as any).commands)).toEqual([]);
  await doc(page).evaluate(() => (window as any).codesense.ready());
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).commands))
    .toMatchObject([{ id: "inspect", args: { stage: "queued" } }]);
  const dockFrame = doc(page);
  await page.evaluate(() => (window as any).switchTeam("ops"));
  await expect.poll(() => dockFrame.evaluate(() => (window as any).ctx?.team?.team)).toBe("ops");
  await page.evaluate(() => {
    (window as any).foldDock();
    (window as any).runDockCommand({ stage: "rebound" });
  });
  await expect(page.getByTestId("agents-dock")).toBeVisible();
  await expect
    .poll(() => dockFrame.evaluate(() => (window as any).commands))
    .toMatchObject([
      { id: "inspect", args: { stage: "queued" } },
      { id: "inspect", args: { stage: "rebound" } },
    ]);
  expect(dockFrame.isDetached()).toBe(false);
  expect(loads.get("dock-await-ready.html")).toBe(1);
});

test("a team switch never reloads the dock frame; each switch re-binds it", async ({ page }) => {
  await open(page);
  await page.frameLocator("iframe").locator("#row").waitFor();
  await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe("dev");
  const teams = ["ops", "dev", "ops", "dev", "ops"];
  for (const [i, team] of teams.entries()) {
    await page.evaluate((t) => (window as any).switchTeam(t), team);
    await expect.poll(() => page.evaluate(() => (window as any).rebinds.length)).toBe(i + 1);
    await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe(team);
  }
  // One document load for the whole run.
  expect(loads.get("dock-ok.html")).toBe(1);
  const rebinds = await page.evaluate(() => (window as any).rebinds);
  // Each re-mint presented the then-current token and got a fresh one.
  expect(rebinds[0].from).toBe("dock-ok.html");
  for (let i = 1; i < rebinds.length; i++) expect(rebinds[i].from).toBe(rebinds[i - 1].to);
  // The shim adopted the last token and the hub asked it to resync.
  const latest = rebinds.at(-1).to;
  expect(await doc(page).evaluate(() => (window as any).__codesenseProxy.token)).toBe(latest);
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).resyncs ?? []))
    .toContain("agentList");
  // A call made with the new token is answered; one sent with an old token is refused.
  expect(
    await doc(page).evaluate(() =>
      (window as any).codesense.invoke("focusTerminal", {}).then(() => "answered"),
    ),
  ).toBe("answered");
  const stale = await doc(page).evaluate(
    (token) =>
      new Promise((done) => {
        addEventListener("message", (e) => {
          if (e.data?.type === "codesense:invoke-result" && e.data.id === 9999)
            done(e.data.ok ? "answered" : `refused: ${e.data.error}`);
        });
        parent.postMessage(
          { token, type: "codesense:invoke", id: 9999, command: "focusTerminal", args: {} },
          "*",
        );
        setTimeout(() => done("ignored"), 1000);
      }),
    rebinds[0].from,
  );
  expect(stale).toMatch(/^(refused: .*denied|ignored$)/);
});

test("cold open paints the first-paint roster before subscribe returns", async ({ page }) => {
  await open(page, "page=dock-ok&slow");
  const frame = page.frameLocator("iframe");
  await expect(frame.locator("#rows li")).toHaveText(["fixture-agent"]);
  expect(await doc(page).evaluate(() => (window as any).rendered)).toEqual(["firstPaint"]);
  await expect(page.getByTestId("pane-startup-placeholder")).toHaveCount(0);
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).rendered))
    .toContain("subscribe");
});

for (const fixture of ["dock-never", "dock-throw"])
  test(`${fixture}: neutral shared placeholder, then the built-in list under the strip`, async ({
    page,
  }) => {
    if (fixture === "dock-never") {
      await page.clock.install({ time: new Date(0) });
      await page.clock.pauseAt(new Date(0));
    }
    await open(page, `page=${fixture}`);
    const cards = page.getByTestId("pane-startup-placeholder");
    if (fixture === "dock-never") {
      await expect(cards).toBeVisible();
      await page.clock.runFor(3500);
      await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
      await page.clock.runFor(26499);
      await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
      await expect(page.locator("[data-slot-id=dock]")).toHaveAttribute("aria-busy", "true");
      await page.clock.runFor(1);
    }
    const strip = page.getByTestId("slot-fallback-strip");
    await expect(strip).toBeVisible({ timeout: 2_500 });
    await expect(page.locator("[data-slot-id=dock]")).not.toHaveAttribute("aria-busy");
    await expect(strip).toContainText(
      "Fixture agents pane didn't load — showing the built-in Agents list.",
    );
    await expect(cards).toHaveCount(0);
    await expect(page.locator("[data-slot-id=dock] .cs-agents")).toBeVisible();
    // The strip sits in a box capped at about 60% of the column.
    const box = (await strip.locator("..").boundingBox())!;
    const column = (await page.getByRole("region", { name: "Agents" }).boundingBox())!;
    expect(box.height).toBeLessThanOrEqual(column.height * 0.6 + 1);
    await strip.getByRole("button", { name: "Details" }).click();
    await expect(strip).toContainText(
      fixture === "dock-never" ? "within 30 seconds" : "hit an error while loading",
    );
    const before = loads.get(`${fixture}.html`) ?? 0;
    await strip.getByRole("button", { name: "Reload" }).click();
    await expect.poll(() => loads.get(`${fixture}.html`) ?? 0).toBeGreaterThan(before);
    if (fixture === "dock-never") await page.clock.runFor(30000);
    await expect(page.getByTestId("slot-fallback-strip")).toContainText("still didn't load", {
      timeout: 4_000,
    });
  });

// SV-PLG-46 (#2559): with the built-in agent-list page listed, a failed dock dashboard falls back
// to it, live in its own frame, under a Retry bar instead of the native list and strip.
test("a failed dock page falls back to the listed built-in agent-list page", async ({ page }) => {
  await open(page, "page=dock-throw&builtin");
  const fallback = page.locator("[data-slot-id=dock] [data-slot-fallback-entry=agent-list]");
  await expect(fallback).toBeVisible();
  await expect(fallback.frameLocator("iframe").locator("#rows li").first()).toHaveText(
    "fixture-agent",
  );
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await expect(page.locator("[data-slot-id=dock] .cs-agents")).toHaveCount(0);
  await expect(fallback.getByRole("button", { name: "Retry" })).toBeVisible();
});

test("Esc in the dock page lands on the focused terminal", async ({ page }) => {
  await open(page);
  await ready(page);
  await doc(page).locator("#second").focus();
  await page.keyboard.press("Escape");
  await expect(page.locator("#terminal")).toBeFocused();
});

test("a declared menu at the 236px dock renders fully and returns the row", async ({ page }) => {
  await open(page);
  await ready(page);
  expect((await page.getByRole("region", { name: "Agents" }).boundingBox())!.width).toBe(236);
  await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.narrow)).toBe(true);
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    void (window as any).codesense
      .showMenu({
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        row: { kind: "agent", sessionId: "fixture-session" },
        items: [{ id: "inspect", label: "Inspectaverylongdashboarditemoutsidethedock" }],
      })
      .then((value: unknown) => ((window as any).menuResult = value));
  });
  const item = page.getByRole("menuitem", { name: "Inspectaverylongdashboarditemoutsidethedock" });
  await expect(item).toBeVisible();
  const bounds = (await item.boundingBox())!;
  expect(bounds.x + bounds.width).toBeGreaterThan(236);
  expect(bounds.x + bounds.width).toBeLessThanOrEqual(await page.evaluate(() => innerWidth));
  await item.click();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toEqual({ itemId: "inspect", row: { kind: "agent", sessionId: "fixture-session" } });
  await expect(doc(page).locator("#second")).toBeFocused();
});

test("the inbox chat popover opens for a dock row and returns focus to its anchor", async ({
  page,
}) => {
  await open(page);
  await ready(page);
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    void (window as any).codesense
      .showPopover({
        kind: "agent-inbox-chat",
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        sessionId: "fixture-session",
      })
      .then((value: unknown) => ((window as any).popoverResult = value));
  });
  await expect(page.getByRole("dialog", { name: /^Messages with / })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).popoverResult))
    .toEqual({ closed: true });
  await expect(doc(page).locator("#second")).toBeFocused();
});

test("builtin:stop in a dock menu raises the host Stop confirmation", async ({ page }) => {
  await open(page);
  await ready(page);
  await doc(page).evaluate(() => {
    const anchor = document.getElementById("second")!;
    void (window as any).codesense
      .showMenu({
        anchorRect: anchor.getBoundingClientRect().toJSON(),
        anchorId: "second",
        row: { kind: "agent", sessionId: "fixture-session" },
        items: [{ id: "builtin:stop" }],
      })
      .then((value: unknown) => ((window as any).menuResult = value));
  });
  await page.getByRole("menuitem", { name: "Stop…" }).click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).menuResult))
    .toMatchObject({ itemId: "builtin:stop", builtin: true });
});

test("a dock row with no menu declaration shows the default agent-row menu", async ({ page }) => {
  await open(page);
  await ready(page);
  await doc(page).locator("#row").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: "Quick view", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(doc(page).locator("#row")).toBeFocused();
});

test("a re-bind keeps the frame registered: no teardown, still ready, focus requests land", async ({
  page,
}) => {
  await open(page);
  await ready(page);
  await page.evaluate(() => (window as any).switchTeam("ops"));
  await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe("ops");
  const received: string[] = await doc(page).evaluate(() => (window as any).received);
  expect(received).not.toContain("codesense:teardown");
  expect(received).not.toContain("codesense:flush");
  await page.locator("#terminal").focus();
  await page.evaluate(() => (window as any).focusDock());
  await expect(page.locator("iframe")).toBeFocused();
});

test("right-pane retirement of the same dashboard never tears down the dock", async ({ page }) => {
  await open(page);
  await ready(page);
  await page.evaluate(() => (window as any).retireRightPane((window as any).entryPanelId));
  await page.waitForTimeout(400);
  expect(await doc(page).evaluate(() => (window as any).received)).not.toContain(
    "codesense:teardown",
  );
  await expect(page.locator("iframe")).toBeVisible();
});

test("switching A → B → A while B's re-bind is pending ends bound to A", async ({ page }) => {
  await open(page, "page=dock-ok&slow-rebind");
  await ready(page);
  await page.evaluate(() => (window as any).switchTeam("ops"));
  await page.waitForTimeout(100);
  await page.evaluate(() => (window as any).switchTeam("dev"));
  await expect.poll(() => page.evaluate(() => (window as any).rebinds.at(-1)?.team)).toBe("dev");
  await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe("dev");
  await page.waitForTimeout(600);
  expect(await doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe("dev");
});

test("a dock dashboard chosen in team A resolves at startup on sibling team B", async ({
  page,
}) => {
  await open(page, "page=dock-ok&team=ops");
  await ready(page);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe("ops");
});

test("editing the dock page reloads it", async ({ page }) => {
  await open(page);
  await ready(page);
  expect(loads.get("dock-ok.html")).toBe(1);
  for (const expectedLoads of [2, 3]) {
    const outgoing = doc(page);
    await page.evaluate(() => (window as any).bumpReload());
    await expect.poll(() => outgoing.isDetached()).toBe(true);
    await ready(page);
    await expect
      .poll(() => doc(page).evaluate(() => (window as any).rendered.includes("subscribe")))
      .toBe(true);
    expect(loads.get("dock-ok.html")).toBe(expectedLoads);
  }
});

test("choosing Built-in after editing preserves the outgoing page until its save finishes", async ({
  page,
}) => {
  await open(page);
  await ready(page);
  const originalFrame = doc(page);
  await page.evaluate(() => (window as any).bumpReload());
  await expect.poll(() => originalFrame.isDetached()).toBe(true);
  await ready(page);
  const editedLoads = loads.get("dock-ok.html")!;
  expect(editedLoads).toBeGreaterThan(1);
  await page.evaluate(() => {
    (window as any).calls = [];
    (window as any).choose("native");
  });
  await expect(page.locator("[data-testid=agents-dock] .cs-agents")).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => (window as any).calls.map(([c]: [string]) => c)))
    .toContain("release_frame");
  expect(loads.get("dock-ok.html")).toBe(editedLoads);
  const order: string[] = await page.evaluate(() =>
    (window as any).calls.map(([c]: [string]) => c),
  );
  expect(order.indexOf("fixture:saved")).toBeGreaterThanOrEqual(0);
  expect(order.indexOf("fixture:saved")).toBeLessThan(order.indexOf("release_frame"));
  await expect
    .poll(() => page.evaluate(() => (window as any).calls.map(([c]: [string]) => c)))
    .toContain("fixture:teardown");
  await expect(page.locator("iframe")).toHaveCount(0);
});

test("a successful Reload moves focus into the dock page", async ({ page, browserName }) => {
  await open(page, "page=dock-flaky");
  const strip = page.getByTestId("slot-fallback-strip");
  await expect(strip).toBeVisible({ timeout: 2_500 });
  await strip.getByRole("button", { name: "Reload" }).click();
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await expect(page.locator("iframe")).toBeFocused();
  // The page is told to take focus (focus-in). WebKit does not yet land it on the first
  // control after a reload; tracked as a known gap.
  await expect
    .poll(() => doc(page).evaluate(() => (window as any).received))
    .toContain("codesense:focus-in");
  if (browserName === "chromium")
    await expect(page.frameLocator("iframe").locator("#row")).toBeFocused();
});

test("a page still loading stays neutral past three seconds and falls back at the absolute thirty-second deadline", async ({
  page,
}) => {
  await page.clock.install({ time: new Date(0) });
  await page.clock.pauseAt(new Date(0));
  await open(page, "page=dock-hang");
  await expect(page.getByTestId("pane-startup-placeholder")).toBeVisible();
  await page.clock.runFor(3500);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await page.clock.runFor(26499);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await page.clock.runFor(1);
  await expect(page.getByTestId("slot-fallback-strip")).toBeVisible();
  await expect(page.locator("[data-slot-id=dock]")).not.toHaveAttribute("aria-busy");
  await expect(page.locator("[data-slot-id=dock] .cs-agents")).toBeVisible();
});

test("a dashboard missing at launch loads after its file is restored and Reload", async ({
  page,
}) => {
  await open(page, "page=dock-ok&missing");
  const strip = page.getByTestId("slot-fallback-strip");
  await expect(strip).toBeVisible();
  await expect(page.locator("[data-slot-id=dock] .cs-agents")).toBeVisible();
  await page.evaluate(() => (window as any).restore());
  await strip.getByRole("button", { name: "Reload" }).click();
  await ready(page);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await expect.poll(() => doc(page).evaluate(() => (window as any).ctx?.team?.team)).toBe("dev");
  expect(
    await page.evaluate(
      () =>
        (window as any).calls.filter(([c]: [string]) => c === "mint_frame")[0]?.[1].args
          .contentRoot,
    ),
  ).toBe("/fixture");
});

test("missing at launch, then Built-in, restored, and chosen again loads the page", async ({
  page,
}) => {
  await open(page, "page=dock-ok&missing");
  await expect(page.getByTestId("slot-fallback-strip")).toBeVisible();
  await page.evaluate(() => (window as any).choose("native"));
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  await expect(page.locator("iframe")).toHaveCount(0);
  await page.evaluate(() => (window as any).restore());
  await page.evaluate(() => (window as any).choose("dashboard"));
  await ready(page);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as any).calls.filter(([c]: [string]) => c === "mint_frame").at(-1)?.[1].args
          .contentRoot,
    ),
  ).toBe("/fixture");
});

test("choosing Built-in retires the dock page through flush, then teardown, then release", async ({
  page,
}) => {
  await open(page);
  await ready(page);
  await page.evaluate(() => (window as any).choose("native"));
  // The built-in list shows at once, while the outgoing page finishes its save.
  await expect(page.locator("[data-testid=agents-dock] .cs-agents")).toBeVisible({ timeout: 80 });
  await expect
    .poll(() => page.evaluate(() => (window as any).calls.map(([c]: [string]) => c)))
    .toContain("release_frame");
  const order: string[] = await page.evaluate(() =>
    (window as any).calls
      .map(([c]: [string]) => c)
      .filter((c: string) => ["fixture:saved", "fixture:teardown", "release_frame"].includes(c)),
  );
  // The save finishes before the grant is released; the page then hears teardown (its echo
  // back to the host can arrive after the release).
  expect(order.indexOf("fixture:saved")).toBeGreaterThanOrEqual(0);
  expect(order.indexOf("fixture:saved")).toBeLessThan(order.indexOf("release_frame"));
  await expect
    .poll(() => page.evaluate(() => (window as any).calls.map(([c]: [string]) => c)))
    .toContain("fixture:teardown");
  await expect(page.locator("iframe")).toHaveCount(0);
});
