// SPDX-License-Identifier: MIT
import { expect, test, type Frame, type Page } from "../support/quarantine-playwright";
import { viewerScript } from "../support/page-shim";

// The reading position and the keyboard target must both survive the host handing focus
// back to an HTML doc: jsdom cannot scroll, so this runs the real shim in a real engine.
const shim = viewerScript("LINK_INTERCEPTOR_JS");

const doc = `<!doctype html><body style="margin:0">
<button id="first" style="display:block;height:30px" onclick="window.activations=(window.activations||0)+1">first</button>
<div style="height:1500px"></div>
<button id="mid" style="display:block;height:30px" onclick="window.activations=(window.activations||0)+1">mid</button>
<div style="height:2000px"></div>
<script>${shim.replace(/<\/script/gi, "<\\/script")}</script></body>`;

async function load(page: Page): Promise<Frame> {
  await page.setViewportSize({ width: 600, height: 700 });
  await page.setContent(
    `<textarea id="cli"></textarea><iframe id="doc" style="width:500px;height:450px;border:0"></iframe>`,
  );
  await page.evaluate((html) => {
    (document.getElementById("doc") as HTMLIFrameElement).srcdoc = html;
  }, doc);
  const frame = await (await page.waitForSelector("#doc")).contentFrame();
  if (!frame) throw new Error("doc frame missing");
  await frame.waitForSelector("#mid", { state: "attached" });
  // Reading position in the middle of the doc; focus then leaves for the CLI.
  await frame.evaluate(() => window.scrollTo(0, 1200));
  await expect.poll(() => frame.evaluate(() => window.scrollY)).toBe(1200);
  await page.focus("#cli");
  return frame;
}

async function hostMessage(page: Page, frame: Frame, data: Record<string, unknown>) {
  // The host focuses the iframe, then posts; the shim only trusts its parent.
  await page.evaluate((message) => {
    const f = document.getElementById("doc") as HTMLIFrameElement;
    f.focus();
    f.contentWindow?.postMessage(message, "*");
  }, data);
  await frame.evaluate(() => new Promise((resolve) => setTimeout(resolve, 100)));
}

async function expectKeyboardSafe(page: Page, frame: Frame) {
  expect(await frame.evaluate(() => window.scrollY)).toBe(1200);
  const active = await frame.evaluate(() => document.activeElement?.tagName);
  expect(["BODY", "HTML"]).toContain(active);
  await page.keyboard.press("Enter");
  expect(await frame.evaluate(() => (window as { activations?: number }).activations ?? 0)).toBe(0);
}

test("focus-in with nothing focused keeps scroll and focuses no control", async ({ page }) => {
  const frame = await load(page);
  await hostMessage(page, frame, { type: "codesense:focus-in" });
  await expectKeyboardSafe(page, frame);
});

test("focus-return without a usable anchor keeps scroll and focuses no control", async ({
  page,
}) => {
  const frame = await load(page);
  await hostMessage(page, frame, { type: "codesense:focus-return", anchorId: "gone" });
  await expectKeyboardSafe(page, frame);
});

test("focus-return restores the previously focused control without scrolling", async ({ page }) => {
  const frame = await load(page);
  await hostMessage(page, frame, { type: "codesense:focus-return", anchorId: "mid" });
  expect(await frame.evaluate(() => window.scrollY)).toBe(1200);
  expect(await frame.evaluate(() => document.activeElement?.id)).toBe("mid");
});
