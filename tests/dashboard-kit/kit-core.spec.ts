// SPDX-License-Identifier: MIT
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";

import {
  bridgeScript,
  hideScript,
  shootStates,
  showScript,
  stateFixture,
  STATES,
  storageScript,
  // @ts-expect-error -- a plain Node ESM script without type declarations.
} from "../../scripts/lib/dashboard-state-fixtures.mjs";

// SV-CDE-86 acceptance, one page against both hosts. The shell frame stands for the app bridge or the share viewer; the page
// frame loads kit.js or cs-live.js and runs the same script. Every wait is the virtual clock: no real-time sleeps.

const ROOT = process.cwd();
const FILES: Record<string, string> = {
  "/kit.js": readFileSync(join(ROOT, "src-tauri/src/http/routes/dashboard_kit/kit.js"), "utf8"),
  "/cs-live.js": readFileSync(
    join(ROOT, "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
    "utf8",
  ),
};
const NOW = Date.parse("2026-10-30T10:00:00Z");
type Host = "app" | "hosted";
type Fixture = ReturnType<typeof stateFixture>;

const ANSWERS = `window.__answers = {
  storyTotals: (args) => ({ stories: Object.fromEntries(args.storyIds.map((id) => [id, { totals: { costUsd: 1 }, computedAtMs: 1 }])), reasons: [] }),
  queryContext: { rows: ["fresh"] },
};
for (let i = 0; i < 6; i++) window.__answers["q" + i] = {};`;

// The page script both hosts run: a 200-row list, a cached query, and (mode=sat) six reads at once.
const PAGE = `
// CS is already a global on both hosts (a share page declares it itself), so the page does not declare it again.
const mode = new URLSearchParams(location.search).get("mode");
const list = document.getElementById("list");
for (let i = 0; i < 200; i++) { const r = document.createElement("div"); r.className = "row"; r.dataset.id = "S-" + i; r.textContent = "S-" + i; list.append(r); }
CS.budget({ inFlight: 2 });
CS.visibleTotals(list, { rowSelector: ".row", root: list });
const q = CS.cachedQuery("queryContext", { limit: 50 }, { key: "story-list", maxAgeMs: 1000 });
const show = () => { const p = q.peek(); document.getElementById("first").textContent = p.state + ":" + (p.error ? "error" : "ok"); };
show();
q.onChange(show);
if (mode === "sat") for (let i = 0; i < 6; i++) CS.cachedQuery("q" + i, {}, { key: "sat", maxAgeMs: 1000 });
`;

const pageHtml = (
  host: Host,
) => `<!doctype html><html><head><style>#list{height:400px;overflow:auto}.row{height:40px}</style></head><body>
<div id="first"></div><div id="list"></div>
${
  host === "app"
    ? `<script>window.codesense = { has: () => true, invoke: (name, args) => window.parent.__bridge(name, args) };</script><script src="/kit.js"></script>`
    : `<script>
// The share viewer's frame script, reduced to its job: the toolkit posts each request to its own window, and this forwards it to the viewer.
window.addEventListener("message", function (e) { if (e.source === window && e.data && e.data.cs === "live") window.parent.postMessage(e.data, "*"); });
</script><script src="/cs-live.js"></script>`
}
<script>${PAGE}</script></body></html>`;

function shellHtml(host: Host, fixture: Fixture, mode: string) {
  const relay =
    host === "app"
      ? ""
      : `const frame = document.getElementById("f");
frame.addEventListener("load", () => frame.contentWindow.postMessage({ cs: "live", v: 1, op: "hello", role: "viewer", control: false }, "*"));
window.addEventListener("message", (e) => {
  const m = e.data;
  if (e.source !== frame.contentWindow || !m || m.cs !== "live" || m.op !== "query") return;
  window.__bridge(m.query, m.args).then(
    (answer) => frame.contentWindow.postMessage({ cs: "live", v: 1, op: "answer", id: m.id, ok: true, answer }, "*"),
    (err) => frame.contentWindow.postMessage({ cs: "live", v: 1, op: "answer", id: m.id, ok: false, error: String(err.message) }, "*"),
  );
});`;
  return `<!doctype html><html><body><script>${ANSWERS}</script><script>${bridgeScript(fixture)}</script>
<iframe id="f" ${host === "hosted" ? 'sandbox="allow-scripts"' : ""} src="/page?host=${host}&mode=${mode}" width="600" height="500"></iframe><script>${relay}</script></body></html>`;
}

async function open(page: Page, host: Host, fixture: Fixture, mode = "list") {
  await page.clock.install({ time: NOW });
  await page.route("http://kit.test/**", (route) => {
    const url = new URL(route.request().url());
    if (FILES[url.pathname])
      return route.fulfill({ contentType: "text/javascript", body: FILES[url.pathname] });
    const body = url.pathname === "/page" ? pageHtml(host) : shellHtml(host, fixture, mode);
    return route.fulfill({ contentType: "text/html", body });
  });
  await page.addInitScript(storageScript(fixture));
  await page.goto("http://kit.test/shell");
  const frame = page.frameLocator("#f");
  await frame.locator("#first").waitFor();
  return frame;
}

// One IntersectionObserver delivery: the rows' visibility has been computed for the current scroll position.
const settled = (page: Page) =>
  page.frames()[1].evaluate(
    () =>
      new Promise<void>((done) => {
        const io = new IntersectionObserver(() => {
          io.disconnect();
          done();
        });
        io.observe(document.getElementById("list")!);
      }),
  );
type Read = { name: string; args: { storyIds?: string[] } };
const reads = (page: Page, name: string) =>
  page.evaluate(
    (n) => (window as unknown as { __reads: Read[] }).__reads.filter((r) => r.name === n),
    name,
  );
// The virtual clock advances in steps while the real message hops (hosted) and observer deliveries catch up.
const until = <T>(page: Page, ms: number, read: () => Promise<T>) =>
  expect.poll(
    async () => {
      await page.clock.runFor(ms);
      return read();
    },
    { timeout: 15_000 },
  );
const text = (frame: ReturnType<Page["frameLocator"]>) => () =>
  frame.locator("#first").textContent();

for (const host of ["app", "hosted"] as const) {
  test.describe(`dashboard kit, ${host} host`, () => {
    test("(a) warm: the cached list paints before any read returns, then revalidates once (hosted: warm within the page's life)", async ({
      page,
    }) => {
      const key = `cs-kit:1:${host}:story-list:queryContext:{"limit":50}`;
      const fx = stateFixture("warm", {
        seed: { [key]: { at: NOW - 5000, value: { rows: ["cached"] } } },
      });
      const frame = await open(page, host, fx);
      if (host === "app") {
        // Painted from storage, with the answer still on its way (the bridge's latency has not run).
        expect(await text(frame)()).toBe("stale:ok");
      } else {
        // A shared page's frame is sandboxed (an opaque origin): it has no storage, so nothing is warm across loads.
        expect(await text(frame)()).toBe("none:ok");
      }
      await expect.poll(async () => (await reads(page, "queryContext")).length).toBe(1);
      await until(page, 450, text(frame)).toBe("fresh:ok");
      expect(await reads(page, "queryContext")).toHaveLength(1);
      if (host === "hosted") {
        // Within the page's life it is warm: the same cachedQuery is the answer already read, with no second read.
        const again = await page.frames()[1].evaluate(
          () =>
            (
              window as unknown as {
                CS: {
                  cachedQuery: (
                    n: string,
                    a: object,
                    o: object,
                  ) => { peek: () => { state: string } };
                };
              }
            ).CS.cachedQuery(
              "queryContext",
              { limit: 50 },
              { key: "story-list", maxAgeMs: 1000 },
            ).peek().state,
        );
        expect(again).toBe("fresh");
        expect(await reads(page, "queryContext")).toHaveLength(1);
      }
    });

    test("(b) a 200-row list asks storyTotals only for the visible ids, and only new ones after a scroll", async ({
      page,
    }) => {
      const frame = await open(page, host, stateFixture("cold"));
      await settled(page);
      await until(
        page,
        200,
        async () => (await reads(page, "storyTotals")).length,
      ).toBeGreaterThanOrEqual(1);
      const first = await reads(page, "storyTotals");
      const firstIds = first.flatMap((r) => r.args.storyIds ?? []);
      expect(firstIds.length).toBeLessThan(20);
      expect(firstIds).not.toContain("S-150");
      await page.clock.runFor(1000);
      await frame.locator("#list").evaluate((el) => {
        el.scrollTop = 4000;
      });
      await settled(page);
      await until(page, 200, async () => (await reads(page, "storyTotals")).length).toBeGreaterThan(
        first.length,
      );
      const later = (await reads(page, "storyTotals"))
        .slice(first.length)
        .flatMap((r) => r.args.storyIds ?? []);
      expect(later.filter((id) => firstIds.includes(id))).toEqual([]);
      expect(later).toContain("S-100");
    });

    test("(c) saturated: six reads at once never put more than 2 in flight", async ({ page }) => {
      await open(page, host, stateFixture("cold"), "sat");
      const asked = async () =>
        (
          await page.evaluate(() =>
            (window as unknown as { __reads: Read[] }).__reads.map((r) => r.name),
          )
        ).filter((n) => /^q\d$/.test(n)).length;
      await until(page, 450, asked).toBe(6);
      expect(
        await page.evaluate(() => (window as unknown as { __maxInFlight: number }).__maxInFlight),
      ).toBeLessThanOrEqual(2);
    });

    test("cold shows nothing, then fresh; offline shows an error state, never a value", async ({
      page,
      browser,
    }) => {
      const frame = await open(page, host, stateFixture("cold"));
      expect(await text(frame)()).toBe("none:ok");
      await until(page, 450, text(frame)).toBe("fresh:ok");

      // Another context: storage is per origin and context, and offline means nothing was ever cached.
      const offline = await (await browser.newContext()).newPage();
      const off = await open(offline, host, stateFixture("offline"));
      await until(offline, 450, text(off)).toBe("none:error");
    });

    test("return: hidden for 10 minutes, then shown, makes zero reads and leaves the DOM identical", async ({
      page,
    }) => {
      const key = `cs-kit:1:${host}:story-list:queryContext:{"limit":50}`;
      const fx = stateFixture("return", {
        seed: { [key]: { at: NOW - 5000, value: { rows: ["cached"] } } },
      });
      const frame = await open(page, host, fx);
      await settled(page);
      await until(page, 450, text(frame)).toBe("fresh:ok");
      await until(
        page,
        450,
        async () => (await reads(page, "storyTotals")).length,
      ).toBeGreaterThanOrEqual(1);
      await page.clock.runFor(1000);
      const all = () =>
        page.evaluate(() => (window as unknown as { __reads: Read[] }).__reads.length);
      const before = { reads: await all(), dom: await frame.locator("body").innerHTML() };
      await page.frames()[1].evaluate(hideScript());
      await page.clock.runFor(fx.hiddenMs);
      await page.frames()[1].evaluate(showScript());
      await page.clock.runFor(1000);
      expect(await all()).toBe(before.reads);
      expect(await frame.locator("body").innerHTML()).toBe(before.dom);
    });

    test("every state of the page has a screenshot", async ({ page }) => {
      const dir = mkdtempSync(join(tmpdir(), "dashboard-kit-shots-"));
      const key = `cs-kit:1:${host}:story-list:queryContext:{"limit":50}`;
      const files: string[] = [];
      for (const state of STATES as string[]) {
        const shot = await page.context().newPage();
        const fx = stateFixture(state, {
          seed: { [key]: { at: NOW - 5000, value: { rows: ["cached"] } } },
        });
        files.push(
          ...(await shootStates(
            shot,
            dir,
            host,
            async () => {
              await open(shot, host, fx);
              await shot.clock.runFor(450);
              // The return state is shot after the page really was hidden for its ten minutes and shown again.
              if (state === "return") {
                await shot.frames()[1].evaluate(hideScript());
                await shot.clock.runFor(fx.hiddenMs);
                await shot.frames()[1].evaluate(showScript());
              }
            },
            [state],
          )),
        );
      }
      expect(files).toHaveLength(STATES.length);
    });
  });
}
