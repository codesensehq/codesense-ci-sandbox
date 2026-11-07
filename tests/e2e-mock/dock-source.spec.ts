// SPDX-License-Identifier: MIT
import { expect, test } from "@playwright/test";

test("sibling dock source pushes body and commands, defers hidden loads, and recovers", async ({
  page,
}) => {
  const html = (title: string, chord: string) => `<!doctype html><title>${title}</title>
    <meta name="codesense:slots" content="dock">
    <meta name="codesense:commands" content='[{"id":"find","label":"Find ${title}","defaultChord":"${chord}","scope":"global"}]'>
    <p>${title} body</p><button>Find</button><script>window.stales=0; addEventListener('message', e => { if(e.data?.type==='codesense:stale') window.stales++ }); codesense.ready()</script>`;
  await page.request.put("/__mock-panes/a/people", { data: html("People", "Alt+P") });
  await page.goto("/?harness=dock-sources");
  const body = () => page.frameLocator('[data-testid="agents-dock"] iframe');
  await expect(body().locator("p")).toHaveText("People body");
  await page.evaluate(() => {
    const w = window as any;
    const runtime = w.__TAURI_INTERNALS__;
    const invoke = runtime.invoke.bind(runtime);
    w.__dockMints = 0;
    w.__dockCommands = [];
    runtime.invoke = (command: string, args: any) => {
      if (command === "mint_frame") w.__dockMints++;
      if (command === "commands_sync") w.__dockCommands = args.commands;
      return invoke(command, args);
    };
    w.__originalDock = document.querySelector('[data-testid="agents-dock"] iframe');
  });
  await page.getByRole("button", { name: "Switch team" }).click();
  await expect(page.getByTestId("source-team")).toHaveText("acme/widgets:b");
  expect(
    await page.evaluate(
      () =>
        (window as any).__originalDock ===
        document.querySelector('[data-testid="agents-dock"] iframe'),
    ),
  ).toBe(true);
  expect(await page.evaluate(() => (window as any).__dockMints)).toBe(0);
  await page.request.put("/__mock-panes/a/people", { data: html("Members", "Alt+M") });
  await expect(body().locator("p")).toHaveText("Members body");
  await expect
    .poll(() =>
      page.evaluate(() => (window as any).__dockCommands.map((c: any) => [c.label, c.binding])),
    )
    .toEqual([["Find Members", "Alt+M"]]);
  await page.getByRole("button", { name: "Fold dock" }).click();
  const mints = await page.evaluate(() => (window as any).__dockMints);
  const url = await page.locator('[data-testid="agents-dock"] iframe').getAttribute("src");
  await page.request.put("/__mock-panes/a/people", { data: html("Hidden update", "Alt+H") });
  await expect
    .poll(() => page.evaluate(() => (window as any).__dockCommands[0]?.label))
    .toBe("Find Hidden update");
  expect(await page.locator('[data-testid="agents-dock"] iframe').getAttribute("src")).toBe(url);
  expect(await page.evaluate(() => (window as any).__dockMints)).toBe(mints);
  expect(
    await body()
      .locator("p")
      .evaluate(() => (window as any).stales),
  ).toBe(1);
  await page.getByRole("button", { name: "Fold dock" }).click();
  await expect(body().locator("p")).toHaveText("Hidden update body");
  await page.request.delete("/__mock-panes/a/people");
  await expect(page.getByTestId("slot-fallback-strip")).toBeVisible();
  await page.request.put("/__mock-panes/a/people", { data: html("Returned", "Alt+R") });
  await expect(body().locator("p")).toHaveText("Returned body");
  await expect(page.getByTestId("slot-fallback-strip")).toHaveCount(0);
});
