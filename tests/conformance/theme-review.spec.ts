// SPDX-License-Identifier: MIT
import { test, expect } from "../pane-harness/harness";
import { DEFAULT_FIXTURES, themeCheck, checkPaneContrast } from "./checks";
import { builtInPaneFiles } from "../../scripts/lib/builtin-panes.mjs";
import { contrastRatio, CONTRAST_THRESHOLDS } from "../../src/shared/lib/color-contrast";
import { hex } from "../pane-harness/colors";

const good = "tests/conformance/fixtures/good.html";

for (const background of ["--cs-bg", "--cs-surface"]) {
  test(`good fixture follows both palettes using ${background}, open and hidden`, async ({
    mountPane,
  }) => {
    test.setTimeout(90_000);
    const pane = await mountPane(good, DEFAULT_FIXTURES);
    await pane.frame.locator("body").evaluate((el, token) => {
      el.style.backgroundColor = `var(${token})`;
    }, background);
    await themeCheck.run(pane);
  });
}

test("file-nav uses its declared sidebar surface for live and hidden theme checks", async ({
  mountPane,
}) => {
  test.setTimeout(90_000);
  const pane = await mountPane(good, { ...DEFAULT_FIXTURES, slotId: "file-nav" });
  await themeCheck.run(pane);
});

test("scheme-only colours fail the same-mode palette change", async ({ mountPane }) => {
  const pane = await mountPane(
    "tests/conformance/fixtures/theme-scheme-only.html",
    DEFAULT_FIXTURES,
  );
  await expect(themeCheck.run(pane)).rejects.toThrow(/nord\/light\b.*body.*did not change/);
});

test("contrast skips opacity groups rather than treating their backgrounds as opaque", async ({
  mountPane,
}) => {
  const pane = await mountPane(good, DEFAULT_FIXTURES);
  await pane.frame.evaluate(() => {
    const card = document.createElement("div");
    card.style.cssText = "opacity: .5; background: var(--cs-fg); color: var(--cs-fg)";
    card.innerHTML = "<p>Text inside a translucent group</p>";
    document.body.append(card);
  });
  expect(await checkPaneContrast(pane.frame, "fixture")).toEqual([]);
});

test("contrast samples painted SVG shapes and skips their non-painting groups", async ({
  mountPane,
}) => {
  const pane = await mountPane(good, DEFAULT_FIXTURES);
  await pane.frame.evaluate(() => {
    const container = document.createElement("div");
    container.innerHTML =
      '<svg width="20" height="20"><g fill="var(--cs-bg)"><path d="M 0 0 L 20 20" fill="none" stroke="var(--cs-fg)" /></g></svg>';
    document.body.append(container);
  });
  expect(await checkPaneContrast(pane.frame, "fixture")).toEqual([]);
});

test("docs panes load the generated kit in standalone previews", async ({ page, fakeHost }) => {
  for (const path of builtInPaneFiles().filter(
    (p: string) => p.startsWith("codesense-docs/") && p.endsWith(".html"),
  )) {
    await page.goto(fakeHost.url(path));
    const fallback = page.locator('link[href="../content/_shared/kit-fallback.css"]');
    await expect(fallback).toHaveCount(1);
    await expect.poll(() => fallback.evaluate((el) => !!(el as HTMLLinkElement).sheet)).toBe(true);
    const colors = await page.locator("body").evaluate((el) => {
      const style = getComputedStyle(el);
      return [style.color, style.backgroundColor];
    });
    const [ink, bg] = await hex(page, colors);
    expect(bg.endsWith("00"), path).toBe(false);
    expect(contrastRatio(ink, bg), path).toBeGreaterThanOrEqual(CONTRAST_THRESHOLDS.text);
  }
});
