// SPDX-License-Identifier: MIT
import { randomUUID } from "node:crypto";
import type { FrameRecord, MintFrameArgs } from "../src/tauri/commands";
import { SLOT_GRANT_CEILINGS } from "../src/features/dashboard/lib/verb-registry.generated";
import { shimSource } from "../tests/support/page-shim";

import type { Plugin, ViteDevServer } from "vite";

/**
 * Mock-mode (`pnpm dev:mock`) stand-in for the pane half of the local HTTP API,
 * so a custom right pane renders, hot-reloads and shows its errors with no Rust
 * backend.
 *
 * - `GET /pane/{content}/{panesDir}/{path}` serves a pane (or a 404) with the
 *   real `window.codesense` shim prepended through the shared reader so the mock
 *   runs the same bridge code the app injects.
 * - `GET /api/v1/sprints` and `GET /api/v1/taxonomy` answer the SDK's
 *   `sprints()` / `taxonomy()` with a fixture sprint registry and the default
 *   status taxonomy (each pane gets a `window.__codesenseProxy` read capability
 *   pointing back at this server, as the app injects one per render).
 * - `GET /__mock-panes` lists the in-memory panes; `PUT`/`DELETE
 *   /__mock-panes/{team|-}/{id}` write or remove one (what an agent's
 *   `pane_write` or a file delete does), then push `mock-panes:changed` to the
 *   page over the HMR socket, where the mock backend re-emits it as the app's
 *   `pane-changed` event.
 *
 * Applied only in mock mode; never part of a build.
 */

/** Panes keyed `${team or "-"}/${id}`; `-` is the private space. */
type PaneStore = Map<string, { html: string; modified: string }>;

const SEED: Array<[string, string]> = [
  [
    "-/hello",
    `<!doctype html><html><head><title>Hello pane</title><meta name="color-scheme" content="light dark"></head>
<body style="font:13px system-ui;margin:12px"><h3>Hello from panes/hello.html</h3><p>Edit the file and this reloads.</p></body></html>`,
  ],
];

/** The sprint registry `sprints()` reads (the `SprintRegistry` wire shape). */
const SPRINTS = {
  activeSprint: "2026-10-A",
  sprints: [
    {
      id: "2026-09-B",
      state: "closed",
      goal: "Panes reach parity with the built-in panels",
      focusEpics: ["20-panes"],
      start: "2026-09-15",
      end: "2026-09-28",
      deliveredPoints: 13,
      carriedPoints: 3,
    },
    {
      id: "2026-10-A",
      state: "active",
      goal: "A board pane that drives the sprint",
      focusEpics: ["20-panes", "21-board"],
      start: "2026-09-29",
      end: "2026-10-12",
      deliveredPoints: 0,
      carriedPoints: 0,
    },
    {
      id: "2026-10-B",
      state: "planning",
      goal: "",
      focusEpics: [],
      start: "2026-10-13",
      end: "2026-10-26",
      deliveredPoints: 0,
      carriedPoints: 0,
    },
  ],
  warnings: [],
};

/** The default status taxonomy `taxonomy()` reads (the `StatusTaxonomy` wire shape). */
const TAXONOMY = {
  states: (
    [
      ["planning", "Planning", "#64748b", "CircleDashed", "not_started"],
      ["todo", "Todo", "#94a3b8", "CircleDot", "not_started"],
      ["in-progress", "In progress", "#0ea5e9", "Circle", "active"],
      ["blocked", "Blocked", "#ef4444", "Ban", "blocked"],
      ["review", "Review", "#8b5cf6", "Eye", "review"],
      ["qa", "QA", "#f59e0b", "ShieldCheck", "review"],
      ["done", "Done", "#10b981", "CheckCircle2", "done"],
    ] as const
  ).map(([id, label, color, icon, role], order) => ({ id, label, color, icon, order, role })),
  warnings: [],
};

function decodeRoot(segment: string): string {
  return Buffer.from(segment.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString("utf8");
}

/** `/mock-ctx/<team>/panes` or `/mock-ctx/private/panes` → the store's layer key. */
function layerOf(panesDir: string): string | null {
  const m = /\/mock-ctx\/([^/]+)\/panes$/.exec(panesDir);
  if (!m) return null;
  return m[1] === "private" ? "-" : m[1];
}

function readBody(req: import("node:http").IncomingMessage): Promise<string> {
  return new Promise((ok, fail) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => ok(body));
    req.on("error", fail);
  });
}

function notify(server: ViteDevServer, team: string, id: string): void {
  server.ws.send({
    type: "custom",
    event: "mock-panes:changed",
    data: { scope: team === "-" ? "private" : "team", team: team === "-" ? undefined : team, id },
  });
}

export function mockPanes(): Plugin {
  const panes: PaneStore = new Map(
    SEED.map(([key, html]) => [key, { html, modified: new Date().toISOString() }]),
  );
  let shim = "";
  const frames = new Map<string, FrameRecord>();
  return {
    name: "codesense-mock-panes",
    apply: (_config, env) => env.mode === "mock",
    configResolved(config) {
      shim = shimSource(config.root);
    },
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = new URL(req.url ?? "/", "http://localhost");
        if (url.pathname === "/__mock-frames" && req.method === "POST") {
          const args = JSON.parse(await readBody(req)) as MintFrameArgs;
          const frameId = randomUUID().replace(/-/g, "");
          const origin = `http://f-${frameId}.localhost:${server.config.server.port}`;
          const layer = layerOf(args.contentRoot.replace(/\/content\/?$/, "/panes"));
          const frame: FrameRecord = {
            kind:
              args.message || args.slot === "message"
                ? "message"
                : layer && layer !== "-"
                  ? "team"
                  : "private",
            frameId,
            origin,
            token: randomUUID(),
            grant: {
              frameId,
              principal: "human",
              dashboard: args.documentPath,
              slot: args.slot,
              team: null,
              scope: "open",
            },
            contentRoot: args.contentRoot,
            documentPath: args.documentPath,
            assetFolder: args.documentPath.slice(0, args.documentPath.lastIndexOf("/")),
            policy: {},
            // The mock's open policy grants the full vocabulary in every slot.
            effectiveGrant: [...SLOT_GRANT_CEILINGS["right-pane"]],
            message: args.message ?? null,
            parentFrame: args.parentFrame ?? null,
          };
          frames.set(frameId, frame);
          res.setHeader("content-type", "application/json");
          res.end(JSON.stringify(frame));
          return;
        }
        const rebind = /^\/__mock-frames\/([^/]+)\/rebind-team$/.exec(url.pathname);
        if (rebind && req.method === "POST") {
          const { token, org, team } = JSON.parse(await readBody(req));
          const frame = frames.get(rebind[1]);
          if (!frame || frame.token !== token || !["dock", "file-nav"].includes(frame.grant.slot)) {
            res.statusCode = 403;
            res.end("denied: frame identity or slot");
            return;
          }
          frame.token = randomUUID();
          frame.grant.team = { org, team };
          res.setHeader("content-type", "application/json");
          res.end(JSON.stringify(frame));
          return;
        }
        if (url.pathname === "/__mock-frames/resolve" && req.method === "POST") {
          const { origin, token } = JSON.parse(await readBody(req));
          const frame = [...frames.values()].find((f) => f.origin === origin && f.token === token);
          res.statusCode = frame ? 200 : 403;
          res.setHeader("content-type", "application/json");
          res.end(JSON.stringify(frame?.grant ?? { error: "denied: frame identity" }));
          return;
        }
        if (url.pathname.startsWith("/__mock-frames/") && req.method === "DELETE") {
          frames.delete(url.pathname.split("/").pop()!);
          res.end();
          return;
        }
        if (url.pathname === "/__mock-panes" && req.method === "GET") {
          res.setHeader("content-type", "application/json");
          res.end(
            JSON.stringify(
              [...panes.entries()].map(([key, p]) => {
                const [team, id] = key.split("/");
                return { team, id, html: p.html, modified: p.modified };
              }),
            ),
          );
          return;
        }
        const write = /^\/__mock-panes\/([^/]+)\/([a-z0-9][a-z0-9-]*)$/.exec(url.pathname);
        if (write && (req.method === "PUT" || req.method === "DELETE")) {
          const key = `${write[1]}/${write[2]}`;
          if (req.method === "PUT") {
            panes.set(key, { html: await readBody(req), modified: new Date().toISOString() });
          } else {
            panes.delete(key);
          }
          notify(server, write[1], write[2]);
          res.statusCode = 204;
          res.end();
          return;
        }
        if (url.pathname === "/api/v1/sprints" || url.pathname === "/api/v1/taxonomy") {
          res.setHeader("content-type", "application/json");
          res.end(JSON.stringify(url.pathname.endsWith("sprints") ? SPRINTS : TAXONOMY));
          return;
        }
        const frame = [...frames.values()].find(
          (frame) => new URL(frame.origin).host === req.headers.host,
        );
        if (/^\/(viewer(?:-baseline)?|pane|message)\//.test(url.pathname) && !frame) {
          res.statusCode = 403;
          res.end();
          return;
        }
        if (/^\/viewer(-baseline)?\//.test(url.pathname)) {
          // Content docs have no mock HTTP viewer; answer plainly rather than let
          // the SPA fallback boot the whole app inside the doc's iframe.
          res.statusCode = 503;
          res.setHeader("content-type", "text/plain");
          res.end("Mock mode serves panes only; content HTML needs the app.");
          return;
        }
        const pane = /^\/pane\/([^/]+)\/([^/]+)\/(.+)$/.exec(url.pathname);
        if (pane) {
          const layer = layerOf(decodeRoot(pane[2]));
          const file = decodeURIComponent(pane[3]);
          const id = file.endsWith(".html") ? file.slice(0, -".html".length) : null;
          const found = layer && id ? panes.get(`${layer}/${id}`) : undefined;
          if (!found) {
            res.statusCode = 404;
            res.end();
            return;
          }
          res.setHeader("content-type", "text/html; charset=utf-8");
          res.setHeader("referrer-policy", "no-referrer");
          res.end(
            req.method === "HEAD"
              ? undefined
              : `<script>window.__codesenseProxy={base:location.origin,token:${JSON.stringify(frame!.token)},readCap:"mock",providers:{}}</script><script>${shim}</script>${found.html}`,
          );
          return;
        }
        next();
      });
    },
  };
}
