// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { expect, test, type Frame, type Page } from "../support/quarantine-playwright";
import { shimSource } from "../support/page-shim";
import { prepareHostPage, startFakeHost, warmHost, type FakeHost } from "./support/fake-host";

const HOST_PAGE = "tests/frame-security/fixtures/dock.html?page=dock-agent-list";
let host: FakeHost;
function doc(page: Page): Frame {
  return page.frames().find((f) => f.url().includes("dock-agent-list.html"))!;
}
const row = (page: Page) => doc(page).locator('[data-agent-id="fixture-session"] .al-main');
test.beforeAll(async ({ browser }) => {
  test.setTimeout(60_000);
  const pane = readFileSync("src/features/shell/slots/__fixtures__/agent-list-pane.html", "utf8"),
    shim = shimSource();
  host = await startFakeHost((req, res) => {
    const asset = req.url?.match(/^\/kit\/v1\/(tokens.css|kit.css|kit.js|virtual-list.js)$/);
    if (asset) {
      res.setHeader("content-type", asset[1].endsWith("css") ? "text/css" : "text/javascript");
      res.end(readFileSync(`src-tauri/src/http/routes/dashboard_kit/${asset[1]}`));
      return;
    }
    res.setHeader("content-type", "text/html");
    res.end(
      `<script>window.__codesenseProxy={token:"dock-agent-list.html"};${shim}</script><link rel="stylesheet" href="/kit/v1/tokens.css"><link rel="stylesheet" href="/kit/v1/kit.css"><script src="/kit/v1/kit.js"></script><script src="/kit/v1/virtual-list.js"></script>${pane}`,
    );
  });
  await warmHost(browser, host, HOST_PAGE, (warm) =>
    warm.frameLocator("iframe").locator(".al-main").waitFor({ timeout: 45_000 }),
  );
});
test.afterAll(async () => {
  await host?.close();
});
test.beforeEach(async ({ page }) => {
  page.on("pageerror", (error) => console.log("Dock fixture error:", error.message));
  await prepareHostPage(page, host.framePort);
  await page.goto(host.url(HOST_PAGE));
  await expect(page.frameLocator("iframe").locator(".al-main")).toBeVisible();
});

test("the real pane's host menu escapes its 236px frame and returns focus to the row", async ({
  page,
}) => {
  expect((await page.getByRole("region", { name: "Agents" }).boundingBox())!.width).toBe(236);
  await row(page).click({ button: "right" });
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  const bounds = (await menu.boundingBox())!;
  expect(bounds.x).toBeGreaterThanOrEqual(0);
  expect(bounds.x + bounds.width).toBeGreaterThan(236);
  expect(bounds.x + bounds.width).toBeLessThanOrEqual(await page.evaluate(() => innerWidth));
  expect(bounds.y + bounds.height).toBeLessThanOrEqual(await page.evaluate(() => innerHeight));
  await expect(page.getByRole("menuitem", { name: "Stop…" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(row(page)).toBeFocused();
});

test("Stop opens the host confirmation and Cancel keeps the session running", async ({ page }) => {
  await row(page).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Stop…" }).click();
  const dialog = page.getByRole("alertdialog");
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(row(page)).toBeVisible();
  expect(
    await page.evaluate(() =>
      (window as any).calls.filter(([command]: [string]) =>
        /^(dashboard_kill_by_handle|terminal_kill)/.test(command),
      ),
    ),
  ).toEqual([]);
});

test("Quick view closes to the row; a vanished row falls back to the first control", async ({
  page,
}) => {
  await row(page).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Quick view", exact: true }).click();
  const quick = page.getByRole("dialog", { name: /Quick view of/ });
  await expect(quick).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(quick).not.toBeVisible();
  await expect(row(page)).toBeFocused();
  await row(page).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Quick view", exact: true }).click();
  await expect(quick).toBeVisible();
  await page.evaluate(() => (window as any).removeAgentRow());
  await expect(row(page)).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(doc(page).locator("#al-arrange")).toBeFocused();
});

test("an unhandled Escape in the real pane returns to the terminal", async ({ page }) => {
  await row(page).focus();
  await page.keyboard.press("Escape");
  await expect(page.locator("#terminal")).toBeFocused();
});

test("workspace card host menu returns focus to its head", async ({ page }) => {
  const workspace = doc(page).locator('[data-row-id="workspace:fixture:dev|terminal"] .al-ws-head');
  const anchor = workspace.locator("..");
  await expect(anchor).toHaveAttribute("id", "workspace:fixture:dev|terminal");
  await workspace.focus();
  await workspace.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: "New group…", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(workspace).toBeFocused();
});
