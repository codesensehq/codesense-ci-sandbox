// SPDX-License-Identifier: MIT
/**
 * The real host components in a real browser, with frames served cross-origin: frames
 * from a Node server on 127.0.0.1, the host page from Vite on localhost. Shared by the
 * frame-security specs and the pane harness.
 */
import type { Browser, Page } from "@playwright/test";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { resolve } from "node:path";
import { createServer as createViteServer, type ViteDevServer } from "vite";
import tailwindcss from "@tailwindcss/vite";

export type FakeHost = {
  framePort: number;
  /** A host page under the repo root, e.g. `tests/frame-security/fixtures/lifecycle.html`. */
  url: (page: string) => string;
  close: () => Promise<void>;
};

export async function startFakeHost(
  serveFrame: (req: IncomingMessage, res: ServerResponse) => void,
): Promise<FakeHost> {
  const frames: Server = createServer((req, res) => {
    res.setHeader("access-control-allow-origin", "*");
    serveFrame(req, res);
  });
  await new Promise<void>((done) => frames.listen(0, "127.0.0.1", done));
  const framePort = (frames.address() as { port: number }).port;
  const vite: ViteDevServer = await createViteServer({
    configFile: false,
    plugins: [tailwindcss()],
    root: process.cwd(),
    server: { port: 0, host: "localhost" },
    resolve: { alias: { "@": resolve("src") } },
    esbuild: { jsx: "automatic" },
    logLevel: "error",
  });
  await vite.listen();
  const hostPort = (vite.httpServer!.address() as { port: number }).port;
  return {
    framePort,
    url: (page) => `http://localhost:${hostPort}/${page}`,
    close: async () => {
      await vite.close();
      await new Promise<void>((done) => frames.close(() => done()));
    },
  };
}

/** Tells the host page where frames live and collects what fixture frames report. */
export async function prepareHostPage(page: Page, framePort: number) {
  // A worker's warm-up page can take activation from the page under test.
  await page.bringToFront();
  await page.addInitScript((port) => {
    (window as any).framePort = port;
    addEventListener("message", (e) => {
      if (e.data?.fixture) (window as any).events?.push(e.data);
    });
  }, framePort);
}

/** Loads the host once so Vite's first transform does not count against a test's timeout. */
export async function warmHost(
  browser: Browser,
  host: FakeHost,
  page: string,
  ready: (page: Page) => Promise<unknown>,
) {
  const warm = await browser.newPage();
  try {
    await prepareHostPage(warm, host.framePort);
    await warm.goto(host.url(page), { waitUntil: "domcontentloaded" });
    await ready(warm);
  } finally {
    await warm.close();
  }
}
