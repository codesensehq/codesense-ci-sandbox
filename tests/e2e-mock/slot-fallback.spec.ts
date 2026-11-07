// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";
import {
  expectStandardFallback,
  expectThrowOnLoadFallback,
} from "../support/slot-fallback-assertions";

for (const slot of ["right-pane", "dock"]) {
  test(`a ${slot} pane calls a generated verb through the mock backend`, async ({ page }) => {
    await page.request.put("/__mock-panes/default/generated-verb", {
      data: `<!doctype html><title>Generated verb</title><button id="archive">Archive</button><output></output><script>
        codesense.ready();
        document.querySelector('button').onclick = async () => {
          try {
            const result = await codesense.archive({messageIds:[2]});
            document.querySelector('output').textContent = JSON.stringify(result);
          } catch (error) {
            document.querySelector('output').textContent = String(error);
          }
        };
      </script>`,
    });
    await page.goto("/?harness=slots&agents=team");
    await expect(page.getByTestId("slot-harness-ready")).toBeVisible();
    await page.evaluate(() => {
      const runtime = (window as any).__TAURI_INTERNALS__;
      const invoke = runtime.invoke.bind(runtime);
      (window as any).__archiveCalls = [];
      runtime.invoke = (command: string, args: unknown) => {
        if (command === "mark_inbox_messages_archived") (window as any).__archiveCalls.push(args);
        return invoke(command, args);
      };
    });
    await page.evaluate(
      (slot) => (window as any).__slotHarness.mount({ slot, id: "generated-verb" }),
      slot,
    );
    const pane = page.frameLocator("iframe");
    await pane.getByRole("button", { name: "Archive", exact: true }).click();
    await expect(pane.locator("output")).toHaveText('{"archived":1}');
    expect(await page.evaluate(() => (window as any).__archiveCalls)).toEqual([
      expect.objectContaining({ messageIds: [2] }),
    ]);
  });
}

test("automatic shim ready is transport only; fallback preserves the native selection", async ({
  page,
}) => {
  await page.clock.install();
  await page.request.put("/__mock-panes/default/slot-fixture", {
    data: "<!doctype html><title>Triage inbox</title><p>Starting</p>",
  });
  await page.goto("/?harness=slots");
  await expect(page.getByTestId("slot-harness-ready")).toBeVisible({ timeout: 15_000 });
  await page.evaluate(() =>
    (window as any).__slotHarness.mount({
      slot: "right-pane",
      id: "slot-fixture",
      surface: "notes",
      conversation: "#qa",
    }),
  );
  await page.clock.fastForward(30000);
  await expectStandardFallback(page, {
    name: "Triage inbox",
    builtIn: "Notes",
    detail: "timed out waiting for codesense.ready()",
    selection: "#qa",
  });
  for (const theme of ["light", "dark"])
    for (const width of [420, 236]) {
      await page.evaluate(
        ({ theme, width }) => {
          document.documentElement.classList.toggle("dark", theme === "dark");
          (document.querySelector('[data-testid="slot-preview"]') as HTMLElement).style.width =
            `${width}px`;
        },
        { theme, width },
      );
      await page.waitForTimeout(250);
      await page
        .getByTestId("slot-preview")
        .screenshot({ path: `/tmp/sv-plg5-cp2-details-${theme}-${width}.png` });
      const strip = page.getByTestId("slot-fallback-strip");
      expect(await strip.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
    }
});

test("one strip in both slots; Reload re-mints, preserves the built-in, then restores the live page and header focus", async ({
  page,
}) => {
  await page.clock.install();
  await page.request.put("/__mock-panes/default/slot-reload", {
    data: "<!doctype html><title>Triage inbox</title><p>Starting</p>",
  });
  await page.goto("/?harness=slots");
  await expect(page.getByTestId("slot-harness-ready")).toBeVisible();
  const mount = (slot: string) =>
    page.evaluate(
      (slot) =>
        (window as any).__slotHarness.mount({
          slot,
          id: "slot-reload",
          surface: "notes",
          conversation: "#qa",
        }),
      slot,
    );
  await mount("right-pane");
  const strip = page.getByTestId("slot-fallback-strip");
  await page.clock.fastForward(30000);
  await expectStandardFallback(page, {
    name: "Triage inbox",
    builtIn: "Notes",
    detail: "timed out waiting for codesense.ready()",
    selection: "#qa",
  });
  await mount("dock");
  await page.clock.fastForward(30000);
  await expectStandardFallback(page, {
    name: "Triage inbox",
    builtIn: "Agents list",
    detail: "What happened",
  });
  await mount("right-pane");
  await page.clock.fastForward(30000);
  await expect(strip).toBeVisible();
  const reload = page.getByRole("button", { name: "Reload", exact: true });
  await reload.click();
  await expect(page.getByRole("button", { name: "Reloading…" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await expect(page.getByTestId("native-selection")).toHaveText("#qa");
  const failedOrigin = await page.locator("iframe").getAttribute("src");
  await page.clock.fastForward(30000);
  await expect(reload).toBeFocused();
  await expect(strip).toContainText("pane still didn't load");
  await page.getByRole("button", { name: "Details", exact: true }).click();
  await expect(strip).toContainText("2 times");
  await page.request.put("/__mock-panes/default/slot-reload", {
    data: "<!doctype html><title>Triage inbox</title><p>Working dashboard</p><script>window.bootId=crypto.randomUUID();setTimeout(()=>codesense.ready(),600)</script>",
  });
  await reload.click();
  await expect(page.getByTestId("native-selection")).toHaveText("#qa");
  await expect(strip).toBeHidden({ timeout: 5000 });
  await expect(page.getByText("Triage inbox pane loaded.", { exact: true })).toBeAttached();
  expect(await page.locator("iframe").getAttribute("src")).not.toBe(failedOrigin);
  await expect(page.locator("[data-panel-header-row]").first()).toBeFocused();
  const frame = page.frames().find((f) => f.url().includes("/pane/"))!;
  const boot = await frame.evaluate(() => (window as any).bootId);
  await page.getByRole("button", { name: "Maximize", exact: true }).click();
  await expect(page.getByTestId("slot-scope-chip")).toHaveText("default team");
  await expect(page.getByRole("button", { name: "Exit full screen (Esc)" })).toBeVisible();
  expect(await frame.evaluate(() => (window as any).bootId)).toBe(boot);
  await page.screenshot({ path: "/tmp/sv-plg5-cp2-overlay-dark.png" });
  await page.getByRole("button", { name: "Exit full screen (Esc)" }).click();
  expect(await frame.evaluate(() => (window as any).bootId)).toBe(boot);
});

test("a script error before live uses the standard strip immediately", async ({ page }) => {
  await page.request.put("/__mock-panes/default/slot-throws", {
    data: "<!doctype html><title>Broken pane</title><script>throw new Error('chart failed')</script>",
  });
  await page.goto("/?harness=slots");
  await expect(page.getByTestId("slot-harness-ready")).toBeVisible();
  await page.evaluate(() =>
    (window as any).__slotHarness.mount({ id: "slot-throws", surface: "notes" }),
  );
  await expectThrowOnLoadFallback(page, {
    name: "Broken pane",
    builtIn: "Notes",
    message: "chart failed",
  });
});

test("visible frames stay pinned beyond the global cap", async ({ page }) => {
  const paneId = `scale-${test.info().parallelIndex}`; // own pane id: parallel tests must not share one mock pane
  await page.request.put(`/__mock-panes/default/${paneId}`, {
    data: "<!doctype html><p>Live</p><script>codesense.ready()</script>",
  });
  await page.goto("/?harness=live-hub");
  await page.waitForFunction(() => !!(window as any).__liveHubMount);
  await page.evaluate((id) => (window as any).__liveHubMount(17, true, false, id), paneId);
  await expect.poll(() => page.frames().filter((f) => f.url().includes("/pane/")).length).toBe(17);
  const origins = await page
    .locator("iframe")
    .evaluateAll((frames) => frames.map((f) => (f as HTMLIFrameElement).src));
  await page.waitForTimeout(500);
  expect(
    await page
      .locator("iframe")
      .evaluateAll((frames) => frames.map((f) => (f as HTMLIFrameElement).src)),
  ).toEqual(origins);
});

test("the real hidden subscribe producer admits before visibility without reading a snapshot", async ({
  page,
}) => {
  await page.request.put("/__mock-panes/default/slot-hidden", {
    data: "<!doctype html><title>Subscribed pane</title><p>Starting</p><script>codesense.subscribe('sessions').then(()=>document.querySelector('p').textContent='Subscribed')</script>",
  });
  await page.goto("/?harness=slots");
  await expect(page.getByTestId("slot-harness-ready")).toBeVisible();
  await page.evaluate(() => {
    const runtime = (window as any).__TAURI_INTERNALS__;
    const invoke = runtime.invoke.bind(runtime);
    (window as any).__slotSnapshotReads = 0;
    runtime.invoke = (command: string, args: unknown) => {
      if (command === "live_hub_subscribe") (window as any).__slotSnapshotReads++;
      return invoke(command, args);
    };
  });
  await page.evaluate(() =>
    (window as any).__slotHarness.mount({ id: "slot-hidden", visible: false }),
  );
  const iframe = page.locator("iframe");
  await expect(iframe).toHaveAttribute("src", /slot-hidden.html/);
  await page.waitForTimeout(3500);
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__slotSnapshotReads)).toBe(0);
  await page.evaluate(() => (window as any).__slotHarness.show(true));
  await expect(page.frameLocator("iframe").getByText("Subscribed", { exact: true })).toBeVisible();
  expect(await page.evaluate(() => (window as any).__slotSnapshotReads)).toBeGreaterThan(0);
});

test("slow external assets do not restart the absolute thirty-second startup deadline", async ({
  page,
}) => {
  await page.clock.install();
  let release!: () => void;
  let requested!: () => void;
  const asset = new Promise<void>((resolve) => {
    release = resolve;
  });
  const seen = new Promise<void>((resolve) => {
    requested = resolve;
  });
  await page.route("**/slow-slot-asset.svg", async (route) => {
    requested();
    await asset;
    await route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>',
    });
  });
  await page.request.put("/__mock-panes/default/slow-assets", {
    data: '<!doctype html><title>Slow assets</title><img src="slow-slot-asset.svg"><p>Starting</p>',
  });
  await page.goto("/?harness=slots");
  await expect(page.getByTestId("slot-harness-ready")).toBeVisible();
  await page.evaluate(() =>
    (window as any).__slotHarness.mount({ id: "slow-assets", surface: "notes" }),
  );
  await seen;
  const strip = page.getByTestId("slot-fallback-strip");
  await page.clock.fastForward(3500);
  await expect(strip).toHaveCount(0);
  release();
  await page.clock.fastForward(26500);
  await expect(strip).toBeVisible();
});
