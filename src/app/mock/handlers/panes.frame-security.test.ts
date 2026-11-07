// SPDX-License-Identifier: MIT
// @vitest-environment node
import { afterEach, expect, it } from "vitest";
import {
  createServer,
  request,
  type Server,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { mockPanes } from "../../../../scripts/vite-plugin-mock-panes";
import type { ViteDevServer, ResolvedConfig } from "vite";
let server: Server;
afterEach(async () => {
  if (server) await new Promise<void>((resolve) => server.close(() => resolve()));
});
it("mock panes mint, serve and revoke registered frames while loopback content is refused", async () => {
  const plugin = mockPanes();
  let middleware!: (req: IncomingMessage, res: ServerResponse, next: () => void) => unknown;
  const config = { server: { port: 0 } };
  if (typeof plugin.configResolved !== "function" || typeof plugin.configureServer !== "function")
    throw new Error("Expected mock server hooks");
  await plugin.configResolved.call({} as never, { root: process.cwd() } as ResolvedConfig);
  await plugin.configureServer.call(
    {} as never,
    {
      config,
      ws: { send() {} },
      middlewares: {
        use(fn: typeof middleware) {
          middleware = fn;
        },
      },
    } as unknown as ViteDevServer,
  );
  server = createServer((req, res) => {
    void middleware(req, res, () => {
      res.statusCode = 404;
      res.end();
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as { port: number }).port;
  config.server.port = port;
  const call = (path: string, method = "GET", body = "", host = `localhost:${port}`) =>
    new Promise<{ status: number; body: string }>((resolve, reject) => {
      const req = request(
        { hostname: "127.0.0.1", port, path, method, headers: { host } },
        (res) => {
          let text = "";
          res.on("data", (chunk) => (text += chunk));
          res.on("end", () => resolve({ status: res.statusCode!, body: text }));
        },
      );
      req.on("error", reject);
      req.end(body);
    });
  const minted = await call(
    "/__mock-frames",
    "POST",
    JSON.stringify({
      documentPath: "/mock-ctx/private/panes/hello.html",
      contentRoot: "/mock-ctx/private/content",
      slot: "right-pane",
    }),
  );
  const frame = JSON.parse(minted.body);
  expect(frame.kind).toBe("private");
  expect(frame.grant.scope).toBe("open");
  const root = (value: string) => Buffer.from(value).toString("base64url");
  const pane = `/pane/${root(frame.contentRoot)}/${root("/mock-ctx/private/panes")}/hello.html`;
  const served = await call(pane, "GET", "", new URL(frame.origin).host);
  expect(served.status).toBe(200);
  expect(served.body).toContain(frame.token);
  expect(served.body).toContain("Hello from panes");
  expect((await call(pane)).status).toBe(403);
  expect((await call("/__mock-panes")).status).toBe(200);
  expect((await call(`/__mock-frames/${frame.frameId}`, "DELETE")).status).toBe(200);
  expect((await call(pane, "GET", "", new URL(frame.origin).host)).status).toBe(403);
});
