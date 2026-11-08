// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { test, expect, type Frame, type Page } from "../support/quarantine-playwright";
import { shimSource } from "../support/page-shim";
import { prepareHostPage, startFakeHost, warmHost, type FakeHost } from "./support/fake-host";
const HOST = "tests/frame-security/fixtures/inbox-menus.html";
let host: FakeHost;
let frame: Frame;
test.beforeAll(async ({ browser }) => {
  test.setTimeout(60_000);
  const shim = shimSource();
  const pane = readFileSync("src/features/shell/right-pane/__fixtures__/inbox-pane.html", "utf8");
  host = await startFakeHost((req, res) => {
    res.setHeader("content-type", "text/html");
    const kit = req.url?.match(/^\/kit\/v1\/([^?]+)/);
    if (kit) {
      res.setHeader("content-type", kit[1].endsWith(".css") ? "text/css" : "text/javascript");
      res.end(readFileSync(`src-tauri/src/http/routes/dashboard_kit/${kit[1]}`));
      return;
    }
    const nested = req.url?.startsWith("/message/");
    const token = nested ? "3" : "inbox-pane.html";
    const body = nested
      ? "<p id='message-body'>Nested message body</p><input id='message-field' value='Nested field'>"
      : pane;
    res.end(
      `<script>window.__codesenseProxy={token:${JSON.stringify(token)}};${nested ? shim.replaceAll("window.parent", "window.top") : shim}</script><link rel='stylesheet' href='/kit/v1/tokens.css'><link rel='stylesheet' href='/kit/v1/kit.css'><script src='/kit/v1/kit.js'></script><script src='/kit/v1/virtual-list.js'></script>${body}`,
    );
  });
  await warmHost(browser, host, HOST, (p) =>
    p.frameLocator("iframe").locator(".ib-row").first().waitFor({ timeout: 45000 }),
  );
});
test.afterAll(async () => host?.close());
test.beforeEach(async ({ page }) => {
  page.on("pageerror", (error) => console.log("Inbox fixture error:", error.message));
  await prepareHostPage(page, host.framePort);
  await page.goto(host.url(HOST));
  frame = page.frames().find((f) => f.url().includes("/pane/"))!;
  if (!frame) {
    await page.frameLocator("iframe").locator(".ib-row").first().waitFor();
    frame = page.frames().find((f) => f.url().includes("/pane/"))!;
  }
  await frame.locator(".ib-row").first().waitFor();
});
const menu = (page: Page, name: string | RegExp) =>
  page.getByRole("menuitem", { name, exact: typeof name === "string" });
async function openMia() {
  await frame.locator(".ib-row").filter({ hasText: "Mia" }).click();
  await frame.locator("#draft").waitFor();
}

test("inbox context menu: conversation", async ({ page }) => {
  await frame
    .locator(".ib-row")
    .filter({ hasText: "Mia" })
    .locator(".prev")
    .click({ button: "right" });
  await expect(menu(page, "Open")).toBeVisible();
  await expect(menu(page, "Reply")).toBeVisible();
  await expect(menu(page, "Archive")).toBeVisible();
  await menu(page, "Reply").click();
  await expect(frame.locator("#draft")).toBeFocused();
});
test("inbox context menu: React… comes first and opens the tapback strip on that message", async ({
  page,
}) => {
  await openMia();
  await frame.locator("#message-1 .bd").click({ button: "right" });
  await expect(page.getByRole("menuitem").first()).toHaveText("React…");
  await menu(page, "React…").click();
  const strip = frame.locator("#message-1 .ib-react");
  await expect(strip).toHaveAttribute("data-open", "true");
  await expect(strip.getByRole("button").first()).toBeFocused();
});
test("inbox context menu: message", async ({ page }) => {
  await openMia();
  const message = frame.locator("#message-1 .bd");
  await message.click({ button: "right" });
  await expect(menu(page, "Copy text")).toBeVisible();
  await expect(menu(page, "Mark as unread")).toBeVisible();
  await menu(page, "Mark as unread").click();
  await expect(frame.locator("#latest-count")).toHaveText("Marked unread");
  await frame.locator("#log").evaluate((el) => el.dispatchEvent(new Event("scroll")));
  await expect
    .poll(() =>
      page.evaluate(() => (window as any).inboxRows.find((m: any) => m.messageId === 1).unread),
    )
    .toBe(true);
  await frame.locator("#latest").click();
  await expect
    .poll(() =>
      page.evaluate(() => (window as any).inboxRows.find((m: any) => m.messageId === 1).unread),
    )
    .toBe(false);
});
test("inbox context menu: attachment", async ({ page }) => {
  await openMia();
  await frame.getByRole("button", { name: "notes/a.md", exact: true }).click({ button: "right" });
  for (const name of ["Open", "Open in new tab", "Copy path"])
    await expect(menu(page, name)).toBeVisible();
  await expect(menu(page, "Reply")).toHaveCount(0);
});
test("inbox context menu: role queue", async ({ page }) => {
  await frame
    .locator(".ib-row")
    .filter({ hasText: "#docs" })
    .locator(".prev")
    .click({ button: "right" });
  for (const name of ["Open queue", "Edit role…", "Archive"])
    await expect(menu(page, name)).toBeVisible();
  await menu(page, "Open queue").click();
  await expect(frame.locator("#thread-name")).toHaveText("#docs");
});
test("inbox context menu: composer", async ({ page }) => {
  await openMia();
  await frame.locator("#draft").fill("Keep my draft");
  await frame.locator("#draft").click({ button: "right" });
  await expect(menu(page, /^Paste/)).toBeVisible();
  await expect(menu(page, "Archive message")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(frame.locator("#draft")).toBeFocused();
  await expect(frame.locator("#draft")).toHaveValue("Keep my draft");
});
test("inbox context menu: empty space", async ({ page }) => {
  await frame.locator("#no-selection p").click({ button: "right" });
  await expect(menu(page, "New message")).toBeVisible();
  await expect(menu(page, "Reload pane")).toBeVisible();
  await expect(menu(page, "Edit source")).toHaveCount(0);
  await menu(page, "New message").click();
  await expect(frame.locator("#thread-name")).toHaveText("New message");
});
test("inbox context menu: selected text", async ({ page }) => {
  await openMia();
  const body = frame.locator("#message-1 .bd");
  await body.scrollIntoViewIfNeeded();
  const point = await body.evaluate((el) => {
    const range = document.createRange();
    range.selectNodeContents(el);
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(range);
    const box = range.getClientRects()[0];
    return { x: box.x + Math.min(5, box.width / 2), y: box.y + box.height / 2 };
  });
  const parentBox = await frame.frameElement().then((el) => el.boundingBox());
  await page.mouse.click(parentBox!.x + point.x, parentBox!.y + point.y, { button: "right" });
  await expect(menu(page, /^Copy/)).toBeVisible();
  await expect(menu(page, "Reply")).toHaveCount(0);
});
test("inbox context menu: nested HTML message", async ({ page }) => {
  await openMia();
  const child = frame.frameLocator("#message-3 iframe");
  await child.locator("#message-body").click({ button: "right" });
  await expect(menu(page, "Open in tab")).toBeVisible();
  await expect(menu(page, "Reply")).toBeVisible();
  await menu(page, "Reply").click();
  await expect(frame.locator("#draft")).toBeFocused();
  await child.locator("#message-field").click({ button: "right" });
  await expect(menu(page, /^Paste/)).toBeVisible();
  await expect(menu(page, "Reply")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(child.locator("#message-field")).toBeFocused();
});

test("inbox context menu: header conversation and composer reset", async ({ page }) => {
  await openMia();
  await frame.locator("#thread-name").click({ button: "right" });
  await expect(menu(page, "Reply")).toBeVisible();
  await expect(menu(page, "Archive")).toBeVisible();
  await page.keyboard.press("Escape");
  await frame.locator("#new-message").click();
  await expect(frame.locator("#thread-head")).not.toHaveAttribute("data-codesense-row");
  // Use header padding so WebKit's word selection keeps its normal text-menu precedence.
  await frame.locator("#thread-head").click({ position: { x: 300, y: 4 } });
  await frame.locator("#thread-head").click({ button: "right", position: { x: 300, y: 4 } });
  await expect(menu(page, "New message")).toBeVisible();
  await expect(menu(page, "Archive")).toHaveCount(0);
});

test("inbox context menu: terminal chip has the agent menu", async ({ page }) => {
  await openMia();
  await frame.getByRole("button", { name: "Go to Mia", exact: true }).click({ button: "right" });
  await expect(menu(page, "Show")).toBeVisible();
  await expect(menu(page, "Archive message")).toHaveCount(0);
});

test("inbox context menu: HTML copies readable paragraphs", async ({ page }) => {
  await page.evaluate(() =>
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async (text: string) => {
          (window as any).copiedText = text;
        },
      },
    }),
  );
  await openMia();
  await frame.frameLocator("#message-3 iframe").locator("#message-body").click({ button: "right" });
  await menu(page, "Copy text").click();
  await expect
    .poll(() => page.evaluate(() => (window as any).copiedText))
    .toBe("Nested message body\n\nSecond\nline.");
});

test("inbox context menu: stacked restore preserves marked unread", async ({ page }) => {
  await frame.frameElement().then((f) =>
    f.evaluate((el) => {
      el.style.width = "600px";
    }),
  );
  await expect(frame.locator("#inbox")).toHaveAttribute("data-layout", "stack");
  await openMia();
  await frame.locator("#message-1 .bd").click({ button: "right" });
  await menu(page, "Mark as unread").click();
  const toggle = () =>
    page
      .locator("iframe")
      .first()
      .evaluate((el) => {
        (el as HTMLIFrameElement).contentWindow!.postMessage(
          { type: "codesense:command", id: "toggle-list" },
          "*",
        );
      });
  await toggle();
  await expect(frame.locator("#inbox")).toHaveAttribute("data-view", "list");
  await toggle();
  await expect(frame.locator("#inbox")).toHaveAttribute("data-view", "thread");
  await expect(frame.locator("#latest-count")).toHaveText("Marked unread");
  await expect
    .poll(() =>
      page.evaluate(() => (window as any).inboxRows.find((m: any) => m.messageId === 1).unread),
    )
    .toBe(true);
  await frame.locator("#latest").click();
  await expect
    .poll(() =>
      page.evaluate(() => (window as any).inboxRows.find((m: any) => m.messageId === 1).unread),
    )
    .toBe(false);
});

test("inbox context menu: cross-team avatars and filter restore", async ({ page }) => {
  await page.goto(host.url(HOST) + "?cross-team");
  await page.frameLocator("iframe").locator(".ib-row").first().waitFor();
  frame = page.frames().find((f) => f.url().includes("/pane/"))!;
  const mia = frame.locator(".ib-row").filter({ hasText: "Mia" });
  await mia.locator(".av").click({ button: "right" });
  await expect(menu(page, "Show")).toBeVisible();
  await page.keyboard.press("Escape");
  await openMia();
  await frame.locator("#thread-avatar .av").click({ button: "right" });
  await expect(menu(page, "Show")).toBeVisible();
  await page.keyboard.press("Escape");
  await frame.locator("#thread-name").click({ button: "right" });
  await menu(page, "Mark unread").click();
  await frame.locator("#team-filter").click();
  await frame.locator("#filter-menu button").filter({ hasText: "web" }).click();
  await expect(frame.locator("#latest-count")).toHaveText("Marked unread");
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as any).inboxRows
          .filter((m: any) => m.toHuman && m.conversation === "Mia")
          .every((m: any) => m.unread),
      ),
    )
    .toBe(true);
  await frame.locator("#thread-name").click({ button: "right" });
  await menu(page, "Open").click();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as any).inboxRows
          .filter((m: any) => m.toHuman && m.conversation === "Mia")
          .every((m: any) => !m.unread),
      ),
    )
    .toBe(true);
});
