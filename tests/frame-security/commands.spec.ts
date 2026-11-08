// SPDX-License-Identifier: MIT
import { shimSource } from "../support/page-shim";
import { expect, test, type Page } from "../support/quarantine-playwright";
import { createServer, type Server } from "node:http";
import { resolve } from "node:path";
import { createServer as createViteServer, type ViteDevServer } from "vite";
import tailwindcss from "@tailwindcss/vite";

let frames: Server;
let vite: ViteDevServer;
let url: string;
let framePort: number;
test.beforeAll(async () => {
  test.setTimeout(60_000);
  const shim = shimSource();
  // An inbox-like slot dashboard: it says ready() a little late, so a command sent
  // while its pane is closed has to wait in the host's queue.
  frames = createServer((_req, res) => {
    res.setHeader("content-type", "text/html");
    res.end(
      `<!doctype html><script>window.__codesenseProxy={token:"inbox"};window.commands=[];${shim}</script><input id="field"><script>codesense.onCommand(function(c){commands.push(c)});setTimeout(function(){codesense.ready()},300)</script>`,
    );
  });
  await new Promise<void>((done) => frames.listen(0, "127.0.0.1", done));
  framePort = (frames.address() as { port: number }).port;
  vite = await createViteServer({
    configFile: false,
    plugins: [tailwindcss()],
    root: process.cwd(),
    server: { port: 0, host: "localhost" },
    resolve: { alias: { "@": resolve("src") } },
    esbuild: { jsx: "automatic" },
    logLevel: "error",
  });
  await vite.listen();
  url = `http://localhost:${(vite.httpServer!.address() as { port: number }).port}/tests/frame-security/fixtures/commands.html`;
});
test.afterAll(async () => {
  await vite?.close();
  await new Promise<void>((done) => (frames ? frames.close(() => done()) : done()));
});

async function delivered(page: Page): Promise<{ id: string; dispatchId: string }[]> {
  const frame = page.frames().find((f) => f.url().includes("/pane/"));
  return frame ? frame.evaluate(() => (window as any).commands) : [];
}

test("the archive chord fires with the pane closed, unfocused and focused, once each", async ({
  page,
}) => {
  test.setTimeout(60_000);
  await page.addInitScript((port) => {
    (window as any).framePort = port;
  }, framePort);
  await page.goto(url);
  await page.locator("#terminal").focus();
  await expect(page.locator("iframe")).toHaveCount(0);

  // Closed: the host reveals the slot, waits for the page's ready(), then delivers once.
  await page.keyboard.press("Meta+Shift+Y");
  await expect(page.locator("iframe")).toHaveCount(1, { timeout: 30_000 });
  await expect.poll(() => delivered(page), { timeout: 10_000 }).toHaveLength(1);
  expect((await delivered(page))[0].id).toBe("archive");

  // Open, focus in the terminal: only the ⌘ chord leaves the PTY.
  await page.locator("#terminal").focus();
  // On macOS ⌃ is shell input; elsewhere Ctrl is the platform's CmdOrCtrl.
  if (await page.evaluate(() => /Mac/i.test(navigator.platform)))
    await page.keyboard.press("Control+Shift+Y");
  await page.keyboard.press("Meta+Shift+Y");
  await expect.poll(() => delivered(page)).toHaveLength(2);

  // Focus inside the pane: the shim forwards the chord and the host dispatches it.
  await page.frameLocator("iframe").locator("#field").focus();
  await page.keyboard.press("Meta+Shift+Y");
  await expect.poll(() => delivered(page)).toHaveLength(3);
  const ids = (await delivered(page)).map((c) => c.dispatchId);
  expect(new Set(ids).size).toBe(3);
});
