// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";

// SV-INP-29: the embed pipe in real browsers. The outer iframe stands for the
// hosted dashboard page (sandbox="allow-scripts": an opaque origin, the CSP of
// the share-site frame); it builds the nested embed iframe from a message body
// and runs the real forwarder. Everything crosses real postMessage.

const ROOT = process.cwd();
const EMBED_JS = readFileSync(
  join(ROOT, "src-tauri/src/dashboard_publish/toolkit/cs-embed.js"),
  "utf8",
);
const STATUS_BOARD = readFileSync(
  join(ROOT, "tests/fixtures/live-embed/status-board.html"),
  "utf8",
);

const PAGE_CSP =
  "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; form-action 'none'";

type Row = {
  id: string;
  relPath: string;
  fileName: string;
  kind: string;
  mtime: number;
  title: string;
  fields: Record<string, string>;
};
const row = (id: string, status: string): Row => ({
  id,
  relPath: `stories/${id}.md`,
  fileName: `${id}.md`,
  kind: "story",
  mtime: 1,
  title: `Story ${id}`,
  fields: { status, priority: "p1", sprint: "1", type: "story", owner: "secret-owner" },
});
const ROWS = [
  row("S-1", "done"),
  row("S-2", "done"),
  row("S-3", "review"),
  row("S-4", "in-progress"),
  row("S-5", "todo"),
];

const GRANT = {
  v: 1,
  sha: "abc",
  queries: ["queryContext"],
  streams: [],
  fields: { queryContext: ["status", "priority", "sprint", "title", "type"] },
};

const json = (v: unknown) => JSON.stringify(v).replace(/</g, "\\u003c");
const script = (text: string) => text.replace(/<\/script/gi, "<\\/script");

function pageHtml(opts: { grant: unknown; role: string | null; body: string }) {
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${PAGE_CSP}"></head><body>
<script>${script(EMBED_JS)}</script>
<script>
// Stands for cs-live.js: it holds the token, the page's own handle on the live pipe.
window.CS = { token: "SECRET-TOKEN-DO-NOT-LEAK" };
window.__calls = [];
window.__stray = 0;
var ROWS = ${json(ROWS)};
var upstream = { query: function (name, args) {
  window.__calls.push({ name: name, args: args });
  return Promise.resolve({ total: ROWS.length, truncated: false, rows: ROWS, groups: null });
} };
// A message that is not an embed envelope reaches the page: count it, never answer it.
window.addEventListener("message", function (e) { if (e.data && e.data.cs === "live") window.__stray++; });
(function () {
  var grant = ${json(opts.grant)};
  var role = ${json(opts.role)};
  if (!CSEmbed.mayRun(grant, role)) { document.body.setAttribute("data-scriptless", "1"); return; }
  var pool = CSEmbed.createPool(upstream);
  var f = document.createElement("iframe");
  f.id = "embed";
  f.setAttribute("sandbox", "allow-scripts");
  f.srcdoc = CSEmbed.buildSrcdoc(${json(opts.body)});
  var fwd = CSEmbed.createForwarder({ pool: pool, grant: grant, role: function () { return role; }, frame: f });
  window.addEventListener("message", fwd.handle);
  document.body.appendChild(f);
})();
</script></body></html>`;
}

async function load(page: Page, opts: Parameters<typeof pageHtml>[0]) {
  const external: string[] = [];
  await page.route(/^https?:/, (route) => {
    external.push(route.request().url());
    return route.abort();
  });
  await page.setContent(
    `<!doctype html><body><iframe id="page" sandbox="allow-scripts" style="width:600px;height:800px"></iframe></body>`,
  );
  await page.evaluate((html) => {
    (document.getElementById("page") as HTMLIFrameElement).srcdoc = html;
  }, pageHtml(opts));
  const outer = page.frameLocator("#page");
  const calls = async () => {
    const frame = page.frames().find((f) => f.parentFrame() === page.mainFrame())!;
    return (await frame.evaluate(() => (window as unknown as { __calls: unknown[] }).__calls)) as {
      name: string;
      args: Record<string, unknown>;
    }[];
  };
  const stray = async () => {
    const frame = page.frames().find((f) => f.parentFrame() === page.mainFrame())!;
    return frame.evaluate(() => (window as unknown as { __stray: number }).__stray);
  };
  return { outer, embed: outer.frameLocator("#embed"), external, calls, stray };
}

test.describe("status board in a real nested frame", () => {
  test("renders live for the owner, through the forwarder, with no network", async ({ page }) => {
    const t = await load(page, {
      grant: { ...GRANT, run: "owner" },
      role: "owner",
      body: STATUS_BOARD,
    });
    await expect(t.embed.locator(".h")).toHaveText("Sprint 1 — live status");
    await expect(t.embed.locator(".sub").first()).toHaveText(
      "5 stories · updates live as stories change",
    );
    await expect(t.embed.locator(".st")).toHaveText(["2Done", "1Review", "1In progress", "1To do"]);
    await expect(t.embed.locator("li")).toHaveText(["Story S-3review", "Story S-4in-progress"]);
    const calls = await t.calls();
    expect(calls).toHaveLength(1);
    expect(calls[0].name).toBe("queryContext");
    expect(calls[0].args).toMatchObject({
      where: { kinds: ["story", "bug", "spike"] },
      filters: [{ key: "sprint", op: "eq", value: "1" }],
      select: ["status", "priority"],
      limit: 400,
    });
    expect(t.external).toEqual([]);
  });

  test("a viewer session gets no scripts and no bridge under run: owner", async ({ page }) => {
    const t = await load(page, {
      grant: { ...GRANT, run: "owner" },
      role: "viewer",
      body: STATUS_BOARD,
    });
    await expect(t.outer.locator("body[data-scriptless]")).toHaveCount(1);
    await expect(t.outer.locator("#embed")).toHaveCount(0);
    expect(await t.calls()).toEqual([]);
  });

  test("run defaults to none: nobody gets scripts, not even the owner", async ({ page }) => {
    const t = await load(page, { grant: GRANT, role: "owner", body: STATUS_BOARD });
    await expect(t.outer.locator("#embed")).toHaveCount(0);
    expect(await t.calls()).toEqual([]);
  });

  test("under run: everyone a viewer reads the board but never acts", async ({ page }) => {
    const t = await load(page, {
      grant: { ...GRANT, run: "everyone" },
      role: "viewer",
      body: STATUS_BOARD,
    });
    await expect(t.embed.locator(".st")).toHaveText(["2Done", "1Review", "1In progress", "1To do"]);
  });
});

// A hostile message: everything an agent-authored script could try against the page.
const HOSTILE = `<div id="out"></div><script>
(function () {
  var r = {};
  function safe(name, fn) { try { r[name] = fn(); } catch (e) { r[name] = "threw:" + e.name; } }
  safe("parentCS", function () { return typeof parent.CS; });
  safe("parentToken", function () { return String(parent.CS.token); });
  safe("parentDocument", function () { return typeof parent.document.body; });
  safe("topDocument", function () { return typeof top.document.body; });
  safe("cookie", function () { return document.cookie; });
  safe("localStorage", function () { return typeof localStorage; });
  safe("hasToken", function () { return JSON.stringify(Object.keys(window)).indexOf("SECRET") >= 0; });
  var replies = [];
  window.addEventListener("message", function (e) { if (e.source === parent) replies.push(e.data); });
  function ask(id, verb, extra) {
    var m = { cs: "embed", v: 1, t: "call", id: id, verb: verb, args: {} };
    for (var k in extra) m[k] = extra[k];
    parent.postMessage(m, "*");
  }
  ask(1, "sendAsHuman", { args: { to: "human", text: "hi" } });
  ask(2, "openWorkspace");
  ask(3, "queryContext", { args: { select: ["owner"] } });
  ask(4, "queryContext", { args: { filters: [{ key: "owner", op: "eq", value: "x" }] } });
  ask(5, "queryContext", { args: { select: ["status"] }, grant: { v: 1, run: "everyone", queries: ["queryContext"], fields: { queryContext: ["owner"] } }, role: "owner", token: "x", embedId: "other" });
  for (var i = 10; i < 24; i++) ask(i, "queryContext", { args: { limit: i } });
  parent.postMessage({ cs: "live", v: 1, op: "query", id: "q1", query: "inbox", args: {} }, "*");
  Promise.all([
    fetch("https://example.org/x").then(function () { return "sent"; }, function () { return "blocked"; }),
    new Promise(function (ok) { var im = new Image(); im.onload = function () { ok("sent"); }; im.onerror = function () { ok("blocked"); }; im.src = "https://example.org/p.png"; }),
    new Promise(function (ok) { try { var x = new XMLHttpRequest(); x.open("GET", "https://example.org/x"); x.onerror = function () { ok("blocked"); }; x.onload = function () { ok("sent"); }; x.send(); } catch (e) { ok("blocked"); } }),
  ]).then(function (net) {
    r.fetch = net[0]; r.image = net[1]; r.xhr = net[2];
    setTimeout(function () {
      r.replies = replies.map(function (m) { return { t: m.t, id: m.id, ok: m.ok, code: m.error && m.error.code }; });
      document.body.setAttribute("data-probes", JSON.stringify(r));
    }, 400);
  });
})();
</script>`;

test.describe("a hostile message in a real nested frame", () => {
  async function probe(page: Page, grantExtra: Record<string, unknown>, role: string) {
    const t = await load(page, { grant: { ...GRANT, ...grantExtra }, role, body: HOSTILE });
    const attr = await t.embed
      .locator("body[data-probes]")
      .getAttribute("data-probes", { timeout: 20_000 });
    return { probes: JSON.parse(attr!), t };
  }

  for (const [run, role] of [
    ["owner", "owner"],
    ["everyone", "viewer"],
    ["everyone", "owner"],
  ] as const) {
    test(`run ${run} / ${role}: cannot reach the page, the token, the network or any action`, async ({
      page,
    }) => {
      const { probes, t } = await probe(page, { run }, role);
      // Opaque origin: no handle on the page, the token, storage or cookies.
      expect(probes.parentCS).toMatch(/threw:SecurityError|threw:TypeError/);
      expect(probes.parentToken).toMatch(/^threw:/);
      expect(probes.parentDocument).toMatch(/^threw:/);
      expect(probes.topDocument).toMatch(/^threw:/);
      // WebKit answers an opaque origin's document.cookie with an empty string; Chromium throws.
      expect(probes.cookie === "" || /^threw:/.test(probes.cookie)).toBe(true);
      expect(probes.localStorage).toMatch(/^threw:/);
      expect(probes.hasToken).toBe(false);
      // No network: CSP default-src 'none' stops fetch, image and XHR; nothing left the browser.
      expect([probes.fetch, probes.image, probes.xhr]).toEqual(["blocked", "blocked", "blocked"]);
      expect(t.external).toEqual([]);
      // The forwarder is the only path, and it only reads what the grant names.
      const byId = Object.fromEntries(probes.replies.map((m: { id: number }) => [m.id, m]));
      for (const id of [1, 2, 3, 4])
        expect(byId[id], `id ${id}`).toMatchObject({
          t: "result",
          ok: false,
          code: "not_declared",
        });
      // Spoofed grant/role/token fields on the envelope changed nothing: this one asks only for status, so it ran.
      expect(byId[5]).toMatchObject({ ok: true });
      const calls = await t.calls();
      expect(calls.every((c) => c.name === "queryContext")).toBe(true);
      for (const call of calls) {
        expect(JSON.stringify(call.args)).not.toContain("owner");
      }
      // Per-embed limit: 10 a minute, so of 15 reads (ids 5, 10-23) five are rate limited.
      const reads = probes.replies.filter((m: { id: number }) => m.id === 5 || m.id >= 10);
      expect(reads.filter((m: { ok: boolean }) => m.ok)).toHaveLength(10);
      expect(reads.filter((m: { code?: string }) => m.code === "rate_limited")).toHaveLength(5);
      // A live-shaped message from the embed is not an embed envelope: never answered.
      expect(probes.replies.some((m: { id?: string }) => m.id === "q1")).toBe(false);
    });
  }

  test("run: owner with a viewer session builds no frame for a hostile message", async ({
    page,
  }) => {
    const t = await load(page, {
      grant: { ...GRANT, run: "owner" },
      role: "viewer",
      body: HOSTILE,
    });
    await expect(t.outer.locator("#embed")).toHaveCount(0);
    expect(await t.calls()).toEqual([]);
    expect(t.external).toEqual([]);
  });
});
