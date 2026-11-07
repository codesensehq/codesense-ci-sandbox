// SPDX-License-Identifier: MIT
import { test, expect } from "@playwright/test";
import { contrastRatio } from "../../src/shared/lib/color-contrast";
import {
  startFakeHost,
  prepareHostPage,
  warmHost,
  type FakeHost,
} from "../frame-security/support/fake-host";
import { hex as cssHex } from "./colors";
let host: FakeHost;
const fixture = "tests/pane-harness/fixtures/appearance-host.html";
test.beforeAll(async ({ browser }) => {
  host = await startFakeHost((_, res) => {
    res.statusCode = 404;
    res.end();
  });
  await warmHost(browser, host, fixture, (page) =>
    page.getByRole("group", { name: "Theme", exact: true }).waitFor(),
  );
});
test.afterAll(async () => host?.close());
test("Appearance uses real palettes, native keyboard radios and browse-origin Undo", async ({
  page,
}) => {
  await prepareHostPage(page, host.framePort);
  await page.setViewportSize({ width: 1080, height: 1000 });
  await page.goto(host.url(fixture));
  const themes = page.getByRole("group", { name: "Theme", exact: true });
  await expect(themes.getByRole("radio")).toHaveCount(6);
  await expect(page.locator("html")).toHaveAttribute("data-app-theme", "codesense");
  await themes.getByRole("radio", { name: "CodeSense", exact: true }).focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("ArrowRight");
  await expect(page.locator("html")).toHaveAttribute("data-app-theme", "github");
  const hex = async (color: string, under?: string) => (await cssHex(page, [color], under))[0];
  const chrome = page.getByTestId("tile-chrome-preview");
  const checkAppSurface = async () => {
    const surface = page.getByTestId("app-surface-preview");
    for (const name of ["accent", "accent-foreground", "ring", "popover"]) {
      const values = await surface.evaluate(
        (el, name) => ({
          actual: getComputedStyle(el).getPropertyValue(`--${name}`).trim(),
          expected: getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim(),
        }),
        name,
      );
      expect(values.actual, name).toBe(values.expected);
    }
    const background = await surface.evaluate((el) => getComputedStyle(el).backgroundColor);
    for (const name of ["Inbox link", "HTML inbox link"]) {
      const ink = await surface
        .getByRole("link", { name, exact: true })
        .evaluate((el) => getComputedStyle(el).color);
      expect(contrastRatio(await hex(ink), await hex(background)), name).toBeGreaterThanOrEqual(
        4.5,
      );
    }
    const input = surface.getByRole("textbox", { name: "Repository URL" });
    for (const field of ["color", "placeholder", "border"] as const)
      await expect
        .poll(
          async () => {
            const styles = await input.evaluate((el) => {
              const s = getComputedStyle(el);
              return {
                color: s.color,
                placeholder: getComputedStyle(el, "::placeholder").color,
                border: s.borderTopColor,
                background: s.backgroundColor,
              };
            });
            return contrastRatio(
              await hex(styles[field]),
              await hex(styles.background, background),
            );
          },
          { message: `Input ${field} contrast` },
        )
        .toBeGreaterThanOrEqual(field === "border" ? 3 : 4.5);
  };
  const checkStartupContrast = async () => {
    for (const name of ["startup-failed-preview", "startup-starting-preview"]) {
      const overlay = page.getByTestId(name).getByTestId("pane-startup-overlay");
      const background = await overlay.evaluate((el) => getComputedStyle(el).backgroundColor);
      for (const text of await overlay.locator("p, button").all()) {
        await expect
          .poll(
            async () => {
              const ink = await text.evaluate((el) => getComputedStyle(el).color);
              return contrastRatio(await hex(ink), await hex(background));
            },
            { message: `${name}: ${await text.textContent()}` },
          )
          .toBeGreaterThanOrEqual(4.5);
      }
    }
    const retry = page.getByRole("button", { name: "Retry", exact: true });
    const colors = await retry.evaluate((el) => ({
      border: getComputedStyle(el).borderTopColor,
      bg: getComputedStyle(el.parentElement!.parentElement!).backgroundColor,
    }));
    expect(contrastRatio(await hex(colors.border), await hex(colors.bg))).toBeGreaterThanOrEqual(3);
  };
  await checkStartupContrast();
  await checkAppSurface();
  const label = page.getByTestId("tile-label-preview");
  const ink = await label.evaluate((el) => getComputedStyle(el).color);
  const ground = await chrome.evaluate((el) => getComputedStyle(el).backgroundColor);
  expect(contrastRatio(await hex(ink), await hex(ground))).toBeGreaterThanOrEqual(4.5);
  const newTerminal = chrome.getByRole("button", { name: "New terminal", exact: true });
  await newTerminal.hover();
  await expect.poll(() => newTerminal.evaluate((el) => getComputedStyle(el).color)).toBe(ink);
  const hoverInk = await newTerminal.evaluate((el) => getComputedStyle(el).color);
  expect(contrastRatio(await hex(hoverInk), await hex(ground))).toBeGreaterThanOrEqual(3);
  await page.getByRole("button", { name: "Undo", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-app-theme", "codesense");
  await expect(themes.getByRole("radio", { name: "CodeSense", exact: true })).toBeFocused();
  await checkAppSurface();
  const checkTooltip = async () => {
    await page.getByRole("button", { name: "Show theme help", exact: true }).hover();
    const tooltip = page.locator('[data-slot="tooltip-content"]');
    await expect(tooltip).toBeVisible();
    const colors = await tooltip.evaluate((el) => {
      const style = getComputedStyle(el);
      return { fg: style.color, bg: style.backgroundColor };
    });
    expect(contrastRatio(await hex(colors.fg), await hex(colors.bg))).toBeGreaterThanOrEqual(4.5);
    await page.keyboard.press("Escape");
    await page.mouse.move(0, 0);
    await expect(tooltip).toBeHidden();
  };
  await checkTooltip();
  await page.locator("main").evaluate((el) => (el.scrollTop = 0));
  await page.screenshot({ path: "/tmp/sv-plg22-appearance-light.png", fullPage: true });
  await page
    .getByRole("group", { name: "Mode", exact: true })
    .getByText("Dark", { exact: true })
    .click();
  await expect
    .poll(() =>
      page
        .locator("html")
        .evaluate((el) =>
          getComputedStyle(el).getPropertyValue("--theme-surface-canvas-bg").trim(),
        ),
    )
    .toBe("#0d1117");
  await checkAppSurface();
  await themes
    .locator("label")
    .filter({ has: page.getByRole("radio", { name: "Catppuccin", exact: true }) })
    .click();
  await expect(page.locator("html")).toHaveClass(/dark/);
  await expect
    .poll(() =>
      page
        .locator("html")
        .evaluate((el) =>
          getComputedStyle(el).getPropertyValue("--theme-surface-canvas-bg").trim(),
        ),
    )
    .toBe("#11111b");
  await checkStartupContrast();
  await checkAppSurface();
  await checkTooltip();
  await page.locator("main").evaluate((el) => (el.scrollTop = 0));
  await page.screenshot({ path: "/tmp/sv-plg22-appearance-dark.png", fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect
    .poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    .toBe(true);
  await page.locator("main").evaluate((el) => (el.scrollTop = 0));
  const modeBox = await page.getByRole("group", { name: "Mode", exact: true }).boundingBox();
  expect(modeBox!.x + modeBox!.width).toBeLessThanOrEqual(390 - 24);
  await page.screenshot({ path: "/tmp/sv-plg22-appearance-narrow.png", fullPage: true });
});
