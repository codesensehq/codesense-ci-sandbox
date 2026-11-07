// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";
import { readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { routeAgentListKit, installAgentListPane } from "./agent-list-support";

const source =
  process.env.AGP9_LIVE_PANE || "src/features/shell/slots/__fixtures__/agent-list-pane.html";

test("Manager dock paints roles and pins and retains rows during real-hub session streaming", async ({
  page,
}, testInfo) => {
  test.setTimeout(60_000);
  const errors: string[] = [],
    consoleLog: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    consoleLog.push(`${message.type()}: ${message.text()}`);
    if (message.type() === "error") errors.push(message.text());
  });
  await page.setViewportSize({ width: 1200, height: 900 });
  await routeAgentListKit(page);
  const paneId = `scale-${test.info().parallelIndex}`; // own pane id: parallel tests must not share one mock pane
  await installAgentListPane(page, source, paneId);
  await page.goto("/?harness=live-hub");
  await page.waitForFunction(() => !!(window as any).__liveHubMount);
  await page.evaluate(async (id) => {
    const fixture = (window as any).__liveHubFixture;
    fixture.startAgentPane({ pinned: [0, 3] });
    for (const i of [0, 3])
      Object.assign(fixture.sessions[i], {
        pinned: true,
        roleHoldings: [{ role: "builder", name: "Builder", label: "Builder" }],
        avatar: "builtin-funny:coffee-bot",
      });
    fixture.sessions[1].subagents = 2;
    const { liveHub } = await import("/src/features/dashboard/lib/live-hub.ts");
    await liveHub.writeUi("ui:dock", "all", {
      arrange: "manager",
      folds: {},
      pins: {
        "acme/widgets:default": [0, 3].map((i) => ({
          kind: "session",
          teamKey: "acme/widgets:default",
          handle: fixture.sessions[i].handle,
          sessionId: fixture.sessions[i].sessionId,
          name: fixture.sessions[i].displayName || fixture.sessions[i].handle,
        })),
      },
      search: { query: "", status: "all" },
      hiddenTeams: [],
    });
    (window as any).__liveHubMount(1, true, false, id);
  }, paneId);
  await page.addStyleTag({
    content: "main > div {height: 800px !important; width:236px !important}",
  });
  await expect
    .poll(() => page.frames().filter((frame) => frame.url().includes("/pane/")).length)
    .toBe(1);
  const frame = page.frames().find((frame) => frame.url().includes("/pane/"))!;
  const diagnostics = () =>
    frame.evaluate(() => ({
      count: document.querySelector("#count")?.textContent,
      rows: document.querySelectorAll(".al-row").length,
      paintedOrdinaryRows: [...document.querySelectorAll(".al-row")].filter((el) => {
        if (el.closest<HTMLElement>(".al-item")?.dataset.rowId?.startsWith("pinned:")) return false;
        const r = el.getBoundingClientRect(),
          style = getComputedStyle(el);
        return (
          r.width > 0 &&
          r.height > 0 &&
          r.bottom > 0 &&
          r.top < innerHeight &&
          style.visibility === "visible"
        );
      }).length,
      sizerHeight: document.querySelector("[data-cs-virtual-list]")?.getBoundingClientRect().height,
      listHeight: document.querySelector("#list")?.getBoundingClientRect().height,
      errors: (window as any).frameErrors || [],
    }));
  await expect(frame.locator("#count")).toHaveText("50");
  await expect(frame.locator("#al-arrange")).toHaveValue("manager");
  await expect(frame.locator('[data-row-id^="pinned:"]')).toHaveCount(2);
  await expect(frame.locator('[data-row-id^="pinned:"]').first()).toBeVisible();
  await expect(frame.locator('[data-agent-id="scale-0"] .al-main').last()).toBeVisible();
  await expect(frame.locator('[data-agent-id="scale-0"] .al-holds').last()).toBeVisible();
  await expect(frame.locator('[data-agent-id="scale-1"] .al-state').last()).toContainText(
    "Working",
  );
  await frame.evaluate(() => {
    const w = window as any;
    w.frameErrors = [];
    addEventListener("error", (event) => w.frameErrors.push(event.message));
    const previous = new Map<string, Element>();
    w.stream = { frames: 0, replaced: 0 };
    w.paints = [];
    let lastStep = -1;
    new MutationObserver(() => {
      const title =
        document.querySelector('[data-agent-id="scale-0"] .al-main')?.getAttribute("title") || "";
      const step = /Stream step (\d+)/.exec(title);
      if (!step || Number(step[1]) === lastStep) return;
      lastStep = Number(step[1]);
      const index = lastStep;
      requestAnimationFrame(() =>
        w.paints.push({ index, time: performance.timeOrigin + performance.now() }),
      );
    }).observe(document.querySelector("#list")!, {
      subtree: true,
      attributes: true,
      attributeFilter: ["title"],
    });
    function sample() {
      w.stream.frames++;
      for (const item of document.querySelectorAll<HTMLElement>(".al-item[data-row-id]")) {
        const id = item.dataset.rowId!;
        if (previous.has(id) && previous.get(id) !== item) w.stream.replaced++;
        previous.set(id, item);
      }
      if (!w.stopStream) requestAnimationFrame(sample);
    }
    sample();
  });
  await page.screenshot({ path: testInfo.outputPath("manager-before-stream.png") });
  const timings: number[] = [],
    sizes: number[] = [],
    subscribeReads: number[] = [];
  for (let i = 0; i < 30; i++) {
    const fired = await page.evaluate((i) => {
      const fixture = (window as any).__liveHubFixture;
      Object.assign(fixture.sessions[0], {
        effectiveStatus: i % 2 ? "thinking" : "tool-running",
        progress: { note: `Stream step ${i}` },
        heartbeatAtMs: Date.now(),
      });
      fixture.reset();
      const start = performance.timeOrigin + performance.now();
      return { start, bytes: fixture.fire() };
    }, i);
    await expect
      .poll(() => frame.evaluate((i) => (window as any).paints.some((p: any) => p.index === i), i))
      .toBe(true);
    timings.push(
      await frame.evaluate(
        ({ i, start }) => (window as any).paints.find((p: any) => p.index === i).time - start,
        { i, start: fired.start },
      ),
    );
    sizes.push(fired.bytes);
    subscribeReads.push(
      await page.evaluate(() => (window as any).__liveHubFixture.counters().subscribeReads),
    );
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(3500);
  const stream = await frame.evaluate(() => {
    (window as any).stopStream = true;
    return (window as any).stream;
  });
  const visible = await diagnostics();
  await expect(frame.locator('[data-agent-id="scale-0"] .al-main').last()).toBeVisible();
  await page.evaluate((id) => (window as any).__liveHubMount(1, false, false, id), paneId);
  await page.waitForTimeout(150);
  await page.evaluate(() => {
    const fixture = (window as any).__liveHubFixture;
    fixture.reset();
    for (let i = 0; i < 5; i++) fixture.fire();
  });
  await page.waitForTimeout(250);
  const hiddenReads = await page.evaluate(() => (window as any).__liveHubFixture.counters().reads);
  const perf = {
    eventPaintP95Ms: timings.sort((a, b) => a - b)[Math.ceil(timings.length * 0.95) - 1],
    maxDeltaBytes: Math.max(...sizes),
    maxSubscribeReadsPerDelta: Math.max(...subscribeReads),
    hiddenFrameFetches: hiddenReads,
  };
  await page.evaluate((id) => (window as any).__liveHubMount(1, true, false, id), paneId);
  await expect(frame.locator(".al-row").first()).toBeVisible();
  const report = JSON.stringify(
    {
      source,
      sha256: createHash("sha256").update(readFileSync(source)).digest("hex"),
      stream,
      perf,
      visible,
      errors,
      consoleLog,
    },
    null,
    2,
  );
  writeFileSync(testInfo.outputPath("diagnostics.json"), report);
  await testInfo.attach("diagnostics", {
    body: report,
    contentType: "application/json",
  });
  await page.screenshot({ path: testInfo.outputPath("manager-after-stream.png") });
  expect(errors).toEqual([]);
  expect(visible.errors).toEqual([]);
  expect(visible.rows).toBeGreaterThan(5);
  expect(visible.paintedOrdinaryRows).toBeGreaterThan(5);
  expect(visible.sizerHeight).toBeGreaterThan(500);
  expect(visible.listHeight).toBeGreaterThan(300);
  expect(stream.frames).toBeGreaterThan(30);
  expect(stream.replaced).toBe(0);
  expect(perf.eventPaintP95Ms).toBeLessThan(150);
  expect(perf.maxDeltaBytes).toBeLessThan(2048);
  expect(perf.maxSubscribeReadsPerDelta).toBe(0);
  expect(perf.hiddenFrameFetches).toBe(0);
});
