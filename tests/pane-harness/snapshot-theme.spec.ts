// SPDX-License-Identifier: MIT
import { mkdirSync, writeFileSync } from "node:fs";
import { test, expect } from "./harness";

test("snapshot after dark to light reopens with only the current first-paint palette", async ({
  page,
  mountPane,
  fakeHost,
}, info) => {
  test.setTimeout(60_000);
  await page.goto(fakeHost.url("tests/pane-harness/fixtures/pane-host.html"));
  const palettes = await page.evaluate(async () => {
    // @ts-expect-error Vite serves this module in the browser.
    const { themeTokens } = await import("/src/themes/index.ts");
    return { dark: themeTokens("codesense", "dark"), light: themeTokens("codesense", "light") };
  });
  const original = await mountPane("src/features/shell/slots/__fixtures__/agent-list-pane.html", {
    slotId: "dock",
    theme: { id: "codesense", scheme: "dark", vars: palettes.dark },
  });
  await original.setTheme({ id: "codesense", scheme: "light", vars: palettes.light });
  const expected = palettes.light["--theme-surface-sidebar-bg"];
  await expect
    .poll(() =>
      original.frame.evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue("--cs-bg").trim(),
      ),
    )
    .toBe(expected);
  const html = await page.evaluate(
    () =>
      new Promise<string>((resolve) => {
        const frame = document.querySelector("iframe")!;
        const receive = (event: MessageEvent) => {
          if (
            event.source !== frame.contentWindow ||
            event.data?.type !== "codesense:snapshot-result" ||
            event.data.id !== 8028
          )
            return;
          removeEventListener("message", receive);
          resolve(event.data.html);
        };
        addEventListener("message", receive);
        frame.contentWindow!.postMessage(
          { type: "codesense:snapshot-request", id: 8028, expectedUrl: frame.src },
          new URL(frame.src).origin,
        );
      }),
  );
  expect(html).not.toContain("codesense-initial-theme");
  expect(html).not.toContain(`--cs-bg:${palettes.dark["--theme-surface-sidebar-bg"]}`);
  expect(await original.frame.locator("#codesense-initial-theme").count()).toBe(1);
  const directory = `/tmp/sv-plg-28/snapshot-theme/${info.project.name}`;
  mkdirSync(directory, { recursive: true });
  const file = `${directory}/captured-pane.html`;
  writeFileSync(file, html);
  await original.close();
  const reopened = await mountPane(file, {
    slotId: "dock",
    theme: { id: "codesense", scheme: "light", vars: palettes.light },
  });
  await expect(reopened.frame.locator("#codesense-initial-theme")).toHaveCount(1);
  expect(
    await reopened.frame.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue("--cs-bg").trim(),
    ),
  ).toBe(expected);
});
