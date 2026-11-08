// SPDX-License-Identifier: MIT
/**
 * A pane test harness: `mountPane(path, fixtures)` loads a pane HTML file into the real
 * host (HtmlViewer, SlotDashboardHost, the confirmation and menu layers) in a real
 * browser, cross-origin, with the real shim and kit the viewer server injects. Fixtures
 * stand in for the app: sessions, inbox, other live-hub topics, the intent, the theme and
 * the frame's grant. The returned handle drives lifecycle events from the host side.
 */
import { test as base, expect, type Frame, type Page } from "../support/quarantine-playwright";
import { existsSync, readFileSync } from "node:fs";
import { basename, dirname, extname, resolve, sep } from "node:path";

import { SLOT_GRANT_CEILINGS } from "../../src/features/dashboard/lib/verb-registry.generated";
import type { AppTheme } from "../../src/themes";
import type { FakeTauriOptions } from "../frame-security/fixtures/fake-tauri";
import { shimSource } from "../support/page-shim";
import {
  prepareHostPage,
  startFakeHost,
  warmHost,
  type FakeHost,
} from "../frame-security/support/fake-host";

type Row = Record<string, unknown> & { id: string };

export type PaneTheme = {
  id?: AppTheme;
  scheme: "dark" | "light";
  /** Host CSS variables (e.g. `--background`, `--primary`) the host reads into `--cs-*`. */
  vars?: Record<string, string>;
};

export type PaneFixtures = Pick<
  FakeTauriOptions,
  | "records"
  | "runs"
  | "runReads"
  | "sessions"
  | "inbox"
  | "topics"
  | "views"
  | "activeTab"
  | "budgetDenied"
  | "confirm"
  | "rollupWindow"
> & {
  slotId?: "right-pane" | "dock" | "file-nav";
  title?: string;
  intent?: Record<string, unknown>;
  theme?: PaneTheme;
  /** The frame's effective grant (capability names) that `codesense.has()` answers from.
   *  Default: the full vocabulary, as the default `open` preset grants. */
  effectiveGrant?: string[];
  width?: number;
  height?: number;
  /** The built-in surface the slot falls back to (`inbox` or `notes`). */
  surface?: string;
  /** The slot's shared `ui:right-pane` state before the pane mounts. */
  ui?: Record<string, unknown>;
};
/** `id` is the mount; the pane is `file` in the panes directory `/fixture/<id>`. */
export type PaneHostFixtures = PaneFixtures & { id: string; file: string };

export type Dispatch = {
  verb: string;
  args: Record<string, unknown>;
  class: string;
  at: number;
  status: string;
};

export type InvocationEvent =
  | { kind: "call"; verb: string; at: number }
  | { kind: "reject"; verb: string; error: string; at: number };

export type MountedPane = {
  page: Page;
  /** The pane's own frame, as first mounted. */
  frame: Frame;
  /** The pane's frame now (a reparent reloads it into a new frame). */
  currentFrame: () => Frame | undefined;
  /** What the probe saw in the current document: a per-load id and the host messages. */
  probe: () => Promise<{
    boot: string;
    messages: string[];
    invocations: InvocationEvent[];
  }>;
  /** Move the pane to the other right-pane section, flushing first, as rearranging does. */
  reparent: () => Promise<void>;
  /** Every verb admission the host asked for, with its class and time (performance.now()). */
  dispatches: () => Promise<Dispatch[]>;
  /** Confirmation answers the host sent: `{confirmId, allow}`. */
  answers: () => Promise<Array<{ confirmId: string; allow: boolean }>>;
  show: (visible: boolean) => Promise<void>;
  close: () => Promise<void>;
  maximize: (overlay: boolean) => Promise<void>;
  focus: () => Promise<void>;
  writeUi: (patch: Record<string, unknown>) => Promise<void>;
  setTeam: (org: string, team: string) => Promise<void>;
  /** Flush every frame, as quitting does, waiting at most `deadlineMs`. */
  flush: (deadlineMs?: number) => Promise<void>;
  setTheme: (theme: PaneTheme) => Promise<void>;
  sendIntent: (intent: Record<string, unknown>) => Promise<void>;
  /** Replace or add a row of a topic and push the delta to the pane. */
  upsert: (topic: string, row: Row) => Promise<void>;
  /** The windows the pane has requested for a topic, in order. */
  windows: (topic: string) => Promise<[number, number][]>;
  /** Verbs the pane called, with their args, in order. */
  verbs: () => Promise<Array<{ verb: string; args: unknown }>>;
};

const HOST_PAGE = "tests/pane-harness/fixtures/pane-host.html";
const KIT = resolve("src-tauri/src/http/routes/dashboard_kit");
const AVATARS = resolve("src/assets/avatar-packs");
// The kit as kit_tags() links it.
const KIT_TAGS =
  '<link rel="stylesheet" href="/kit/v1/tokens.css"><link rel="stylesheet" href="/kit/v1/kit.css">' +
  '<script src="/kit/v1/kit.js"></script><script src="/kit/v1/virtual-list.js"></script>';
// Test-only: what this document heard from the host, and an id that changes on every load.
const PROBE =
  "<script>window.__harness={boot:Math.random().toString(36).slice(2),messages:[],invocations:[]};" +
  "addEventListener('message',function(e){var d=e.data;if(e.source===parent&&d&&typeof d.type==='string')__harness.messages.push(d.type==='codesense:visibility'?d.type+':'+(d.visible!==false):d.type)})</script>";
// Observe SDK call/rejection order before the pane runs. Calls started before a denial
// reaches the SDK are already in flight. Both timestamps use this frame's performance clock.
const INVOCATION_PROBE = `<script>(function () {
  var cs = window.codesense, invoke = cs.invoke;
  cs.invoke = function (verb, args) {
    window.__harness.invocations.push({kind: 'call', verb: verb, at: performance.now()});
    return invoke.call(this, verb, args).catch(function (error) {
      window.__harness.invocations.push({kind: 'reject', verb: verb, at: performance.now(), error: String(error.message || error)});
      throw error;
    });
  };
})()</script>`;
const TYPES: Record<string, string> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".jpg": "image/jpeg",
  ".woff2": "font/woff2",
  ".wasm": "application/wasm",
  ".ts": "application/typescript",
};
/** Each mount's panes directory (`/fixture/<mount>`, as the host names it) → the pane's
 *  own directory on disk, so its relative assets resolve as they do in production. */
const mounts = new Map<string, string>();

function inside(root: string, rel: string): string | undefined {
  const file = resolve(root, rel);
  return file.startsWith(root + sep) && existsSync(file) ? file : undefined;
}

/** `/kit/v1/avatars/<pack>/<face>`: a shipped face by its id, through the pack manifest. */
function avatar(path: string): string | undefined {
  const [pack, face] = path.slice("/kit/v1/avatars/".length).split("/");
  if (!path.startsWith("/kit/v1/avatars/") || !pack || !face) return undefined;
  const manifest = inside(AVATARS, `${pack}/pack.json`);
  if (!manifest) return undefined;
  const { faces } = JSON.parse(readFileSync(manifest, "utf8")) as {
    faces: { id: string; file: string }[];
  };
  const file = faces.find((f) => f.id === face.replace(/\.svg$/, ""))?.file;
  return file ? inside(AVATARS, `${pack}/${file}`) : undefined;
}

function servePane(url: string): { type: string; body: string | Buffer } | undefined {
  const path = decodeURIComponent(url.split("?")[0]);
  const kit = path.startsWith("/kit/v1/");
  // Otherwise /pane/<content>/<panesDir as base64url>/<path inside it>.
  const [, route, , panes, ...rest] = path.split("/");
  const dir =
    route === "pane" ? mounts.get(Buffer.from(panes ?? "", "base64url").toString()) : undefined;
  const file = kit
    ? (avatar(path) ?? inside(KIT, path.slice("/kit/v1/".length)))
    : dir
      ? inside(dir, rest.join("/"))
      : undefined;
  if (!file) return undefined;
  const type = TYPES[extname(file)] ?? "application/octet-stream";
  if (kit || type !== "text/html") return { type, body: readFileSync(file) };
  const initialTheme = new URL(url, "http://fixture").searchParams.get("theme");
  const theme = initialTheme ? JSON.parse(initialTheme) : null;
  const style = theme
    ? `<style id="codesense-initial-theme">:root:root{color-scheme:${theme.scheme};${Object.entries(
        theme.tokens,
      )
        .map(([name, value]) => `${name}:${value};`)
        .join("")}}html,body{background:var(--cs-bg);color:var(--cs-fg)}</style>`
    : "";
  return {
    type,
    body:
      style +
      `<script>window.__codesenseProxy={token:${JSON.stringify(basename(file))}}</script>` +
      PROBE +
      `<script>${shimSource()}</script>${INVOCATION_PROBE}${KIT_TAGS}` +
      readFileSync(file, "utf8"),
  };
}

export const test = base.extend<
  {
    mountPane: (
      path: string,
      fixtures?: PaneFixtures,
      options?: { awaitFrame?: boolean },
    ) => Promise<MountedPane>;
  },
  { fakeHost: FakeHost }
>({
  fakeHost: [
    async ({ browser }, provide) => {
      const host = await startFakeHost((req, res) => {
        const found = servePane(req.url ?? "/");
        if (!found) {
          res.statusCode = 404;
          res.end();
          return;
        }
        res.setHeader("content-type", found.type);
        res.end(found.body);
      });
      await warmHost(browser, host, HOST_PAGE, (warm) =>
        warm.waitForFunction(() => !!(window as any).control, null, { timeout: 45_000 }),
      );
      await provide(host);
      await host.close();
    },
    { scope: "worker", timeout: 60_000 },
  ],
  mountPane: async ({ page, fakeHost }, provide) => {
    page.on("pageerror", (error) => console.log("Pane host error:", error.message));
    await provide(async (path, fixtures = {}, options = {}) => {
      const id = `mount-${mounts.size + 1}`;
      mounts.set(`/fixture/${id}`, dirname(resolve(path)));
      const encoded = Buffer.from(`/fixture/${id}`).toString("base64url");
      await prepareHostPage(page, fakeHost.framePort);
      await page.addInitScript(
        (f) => {
          (window as any).paneFixtures = f;
        },
        {
          effectiveGrant: [...SLOT_GRANT_CEILINGS[fixtures.slotId ?? "right-pane"]],
          ...fixtures,
          id,
          file: basename(path),
        } satisfies PaneHostFixtures,
      );
      await page.goto(fakeHost.url(HOST_PAGE), {
        waitUntil: options.awaitFrame === false ? "domcontentloaded" : "load",
      });
      const ours = (f: Frame) => f.url().includes(`/${encoded}/`) && !f.isDetached();
      const currentFrame = () => page.frames().find(ours);
      if (options.awaitFrame !== false) {
        await expect.poll(() => page.frames().some(ours)).toBe(true);
        await currentFrame()!.waitForFunction(() => !!(window as any).codesense?.kit);
      }
      const frame = currentFrame()!;
      const control = (name: string, ...args: unknown[]) =>
        page.evaluate(([n, a]) => (window as any).control[n as string](...(a as unknown[])), [
          name,
          args,
        ] as const);
      const hostState = <T>(name: string) => page.evaluate((n) => (window as any)[n] as T, name);
      return {
        page,
        frame,
        currentFrame,
        probe: () => currentFrame()!.evaluate(() => (window as any).__harness),
        reparent: () => control("reparent"),
        dispatches: () => hostState<Dispatch[]>("dispatches"),
        answers: () => hostState<Array<{ confirmId: string; allow: boolean }>>("answers"),
        show: (visible) => control("show", visible),
        close: async () => {
          await control("flushAll", 300);
          await control("setOpen", false);
        },
        maximize: (overlay) => control("maximize", overlay),
        focus: () => control("focus"),
        writeUi: (patch) => page.evaluate((p) => (window as any).writeUi(p), patch),
        setTeam: (org, team) => control("setTeam", org, team),
        flush: (deadlineMs = 500) => control("flushAll", deadlineMs),
        setTheme: (theme) => control("setTheme", theme),
        sendIntent: (intent) => control("sendIntent", intent),
        upsert: (topic, row) =>
          page.evaluate(
            ([t, r]) => {
              if (t === "records:notes") return (window as any).noteFixture.create(r);
              (window as any).upsert(t, r);
              // The real inbox store emits both live rows and its committed change, which
              // names the message rows it touched.
              if (t === "inbox")
                (window as any).inboxChanged(r.team, r.rowType === "message" ? [r.messageId] : []);
            },
            [topic, row] as const,
          ),
        windows: (topic) =>
          page.evaluate(
            (t) =>
              (window as any).windows
                .filter((w: [string, unknown]) => w[0] === t)
                .map((w: [string, unknown]) => w[1]),
            topic,
          ),
        verbs: () =>
          page.evaluate(() =>
            (window as any).calls
              .filter((c: [string, any]) => c[0] === "verb_dispatch")
              .map((c: [string, any]) => ({ verb: c[1].verb, args: c[1].args })),
          ),
      };
    });
  },
});
export { expect };
