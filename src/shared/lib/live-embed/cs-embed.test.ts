// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { loadShim } from "../../../../tests/support/page-shim";

// SV-INP-29: one live message, three transports. The Sprint 1 status board (msg 13407)
// runs byte-for-byte unchanged under (1) today's in-app shim, (2) the direct hosted
// transport and (3) the nested postMessage transport behind the forwarder, and must
// draw the same page. The forwarder's own rules follow.

const ROOT = process.cwd();
const EMBED_JS = readFileSync(
  join(ROOT, "src-tauri/src/dashboard_publish/toolkit/cs-embed.js"),
  "utf8",
);
const FIXTURE = readFileSync(join(ROOT, "tests/fixtures/live-embed/status-board.html"), "utf8");
const FIXTURE_SCRIPT = FIXTURE.match(/<script>([\s\S]*)<\/script>/)![1];

type Row = {
  id: string;
  relPath: string;
  fileName: string;
  kind: string;
  mtime: number;
  title: string;
  fields: Record<string, string>;
};
type Answer = { total: number; truncated: boolean; rows: Row[]; groups: null };
type CsEmbed = {
  parseGrant: (v: unknown) => Record<string, unknown> | null;
  mayRun: (grant: unknown, role: string | null) => boolean;
  createPool: (u: Upstream, o?: Record<string, unknown>) => Pool;
  createForwarder: (o: Record<string, unknown>) => Forwarder;
  directTransport: (p: Pool, g: unknown, role?: () => string | null) => Transport;
  nestedTransport: (parent: unknown, host: unknown) => Transport;
  createSdk: (t: Transport) => Record<string, (...a: unknown[]) => unknown>;
  buildSrcdoc: (body: string, o?: Record<string, unknown>) => string;
  source: string;
};
type Upstream = { query: (name: string, args: Record<string, unknown>) => Promise<unknown> };
type Pool = { ask: (v: string, a: unknown) => Promise<unknown>; _stats: () => { polls: number } };
type Forwarder = {
  handle: (e: unknown) => boolean;
  setVisible: (v: boolean) => void;
  destroy: () => void;
};
type Transport = { caps: () => string[] };

function loadEmbed(): CsEmbed {
  return new Function(`${EMBED_JS}\nreturn CSEmbed;`)() as CsEmbed;
}

const GRANT = {
  v: 1,
  sha: "abc",
  run: "owner",
  queries: ["queryContext"],
  streams: [],
  fields: { queryContext: ["status", "priority", "sprint", "title", "type"] },
};

function row(id: string, status: string, extra: Record<string, string> = {}): Row {
  return {
    id,
    relPath: `stories/${id}.md`,
    fileName: `${id}.md`,
    kind: "story",
    mtime: 1,
    title: `Story ${id}`,
    fields: { status, priority: "p1", sprint: "1", type: "story", ...extra },
  };
}

const DATA_A = [
  row("S-1", "done"),
  row("S-2", "done"),
  row("S-3", "review"),
  row("S-4", "in-progress"),
  row("S-5", "todo"),
];
const DATA_B = [...DATA_A, row("S-6", "in-progress")];

/** What the page shows, independent of the transport that fed it. */
function summarize(root: Element) {
  return {
    title: root.querySelector(".h")?.textContent,
    sub: root.querySelector(".sub")?.textContent,
    stats: [...root.querySelectorAll(".st")].map((s) => s.textContent),
    bar: [...root.querySelectorAll(".bar span")].map((s) => (s as HTMLElement).style.width),
    list: [...root.querySelectorAll("li")].map((l) => l.textContent),
  };
}

function answer(rows: Row[]): Answer {
  return { total: rows.length, truncated: false, rows, groups: null };
}

/** The Mac's side, enough of it: applies where.kinds and eq filters, as ContextArgs now does. */
function macQuery(rows: () => Row[]) {
  return vi.fn(async (name: string, args: Record<string, unknown>) => {
    expect(name).toBe("queryContext");
    const allowed = ["match", "sort", "limit", "groupBy", "where", "filters", "select"];
    for (const key of Object.keys(args)) expect(allowed).toContain(key);
    const kinds = (args.where as { kinds?: string[] } | undefined)?.kinds;
    const filters = (args.filters as { key: string; value: string }[] | undefined) ?? [];
    return answer(
      rows().filter(
        (r) =>
          (!kinds || kinds.includes(r.kind)) && filters.every((f) => r.fields[f.key] === f.value),
      ),
    );
  });
}

function mount() {
  document.body.innerHTML = "";
  const app = document.createElement("div");
  document.body.append(app);
  app.innerHTML = FIXTURE.replace(/<script>[\s\S]*<\/script>/, "");
  return app.querySelector("#app")!;
}

function runFixture(codesense: unknown) {
  (window as unknown as { codesense: unknown }).codesense = codesense;
  new Function(FIXTURE_SCRIPT)();
}

const flush = async () => {
  for (let i = 0; i < 8; i++) await Promise.resolve();
};

/** An embed frame and its parent, wired to each other by hand (no real window). */
function nestedPair(
  embed: CsEmbed,
  grant: unknown,
  upstream: Upstream,
  pool?: Pool,
  role: () => string | null = () => "owner",
) {
  const sharedPool = pool ?? embed.createPool(upstream);
  const listeners: ((e: { source: unknown; data: unknown }) => void)[] = [];
  const hostWindow = {
    addEventListener: (_: string, fn: (e: { source: unknown; data: unknown }) => void) =>
      listeners.push(fn),
  };
  const embedWindow = {
    postMessage: (data: unknown) => listeners.forEach((fn) => fn({ source: parentWindow, data })),
  };
  const frame = { contentWindow: embedWindow };
  const fwd = embed.createForwarder({ pool: sharedPool, grant, role, frame, embedId: "m1" });
  const parentWindow = {
    postMessage: (data: unknown) => fwd.handle({ source: embedWindow, data }),
  };
  const transport = embed.nestedTransport(parentWindow, hostWindow);
  return { fwd, transport, frame, embedWindow, parentWindow, pool: sharedPool };
}

describe("status board under three transports", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] }));
  afterEach(() => vi.useRealTimers());

  async function expectedFromInApp() {
    const root = mount();
    const shim = loadShim();
    runFixture(shim.codesense);
    shim.deliver({ type: "codesense:visibility", visible: true });
    await flush();
    const invokes = () => shim.posted.filter((m) => m.type === "codesense:invoke");
    expect(invokes()).toHaveLength(1);
    const call = invokes()[0] as { id: number; command: string; args: unknown };
    expect(call.command).toBe("queryContext");
    shim.deliver({ type: "codesense:invoke-result", id: call.id, ok: true, data: answer(DATA_A) });
    await flush();
    return { summary: summarize(root), shim, root };
  }

  it("in-app: today's shim draws the board", async () => {
    const { summary } = await expectedFromInApp();
    expect(summary.title).toBe("Sprint 1 — live status");
    expect(summary.sub).toBe("5 stories · updates live as stories change");
    expect(summary.stats).toEqual(["2Done", "1Review", "1In progress", "1To do"]);
    expect(summary.list).toEqual(["Story S-3review", "Story S-4in-progress"]);
  });

  it("hosted direct: same page, same query reaching the Mac in its local shape", async () => {
    const baseline = (await expectedFromInApp()).summary;
    const embed = loadEmbed();
    const mac = macQuery(() => DATA_A);
    const pool = embed.createPool({ query: mac });
    const root = mount();
    runFixture(embed.createSdk(embed.directTransport(pool, GRANT, () => "owner")));
    await flush();
    expect(summarize(root)).toEqual(baseline);
    expect(mac).toHaveBeenCalledTimes(1);
    expect(mac.mock.calls[0][1]).toMatchObject({
      where: { kinds: ["story", "bug", "spike"] },
      filters: [{ key: "sprint", op: "eq", value: "1" }],
      select: ["status", "priority"],
      limit: 400,
    });
  });

  it("nested: the embed's calls cross the forwarder and draw the same page", async () => {
    const baseline = (await expectedFromInApp()).summary;
    const embed = loadEmbed();
    const mac = macQuery(() => DATA_A);
    const { transport } = nestedPair(embed, GRANT, { query: mac });
    const root = mount();
    runFixture(embed.createSdk(transport));
    await flush();
    expect(summarize(root)).toEqual(baseline);
  });

  it("updates: hosted transports redraw on the shared poll, in-app on a context change", async () => {
    const embed = loadEmbed();
    let data = DATA_A;
    const mac = macQuery(() => data);
    const { transport } = nestedPair(embed, GRANT, { query: mac });
    const root = mount();
    runFixture(embed.createSdk(transport));
    await flush();
    expect(summarize(root).sub).toBe("5 stories · updates live as stories change");
    data = DATA_B;
    await vi.advanceTimersByTimeAsync(15_000);
    await flush();
    expect(summarize(root).sub).toBe("6 stories · updates live as stories change");

    const rootApp = mount();
    const shim = loadShim();
    runFixture(shim.codesense);
    shim.deliver({ type: "codesense:visibility", visible: true });
    await flush();
    const invokes = () => shim.posted.filter((m) => m.type === "codesense:invoke");
    const call = invokes()[0] as { id: number };
    shim.deliver({ type: "codesense:invoke-result", id: call.id, ok: true, data: answer(DATA_A) });
    await flush();
    shim.deliver({ type: "codesense:context-changed", orgs: [], allOrgs: true });
    await flush();
    const again = invokes()[1] as { id: number };
    shim.deliver({ type: "codesense:invoke-result", id: again.id, ok: true, data: answer(DATA_B) });
    await flush();
    expect(summarize(rootApp).sub).toBe("6 stories · updates live as stories change");
  });

  it("errors arrive as (res, err) with the same words on every transport", async () => {
    const embed = loadEmbed();
    const upstream = { query: vi.fn().mockRejectedValue(new Error("mac_offline")) };
    const { transport } = nestedPair(embed, GRANT, upstream);
    const root = mount();
    runFixture(embed.createSdk(transport));
    await flush();
    expect(root.textContent).toBe("Couldn’t load stories: mac_offline");

    const shimRoot = mount();
    const shim = loadShim();
    runFixture(shim.codesense);
    shim.deliver({ type: "codesense:visibility", visible: true });
    await flush();
    const call = shim.posted.find((m) => m.type === "codesense:invoke") as { id: number };
    shim.deliver({ type: "codesense:invoke-result", id: call.id, ok: false, error: "mac_offline" });
    await flush();
    expect(shimRoot.textContent).toBe(root.textContent);
  });

  it("openFile is unavailable on hosted transports, and has() says so", async () => {
    const embed = loadEmbed();
    const sdk = embed.createSdk(
      embed.directTransport(embed.createPool({ query: macQuery(() => DATA_A) }), GRANT),
    );
    expect(sdk.has("openFile")).toBe(false);
    expect(sdk.has("queryContext")).toBe(true);
    await expect(sdk.openFile({ path: "x" })).rejects.toMatchObject({ code: "unavailable" });
  });
});

describe("the forwarder", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] }));
  afterEach(() => vi.useRealTimers());

  function harness(
    grant: unknown = GRANT,
    rows: () => Row[] = () => DATA_A,
    role: () => string | null = () => "owner",
  ) {
    const embed = loadEmbed();
    const mac = macQuery(rows);
    const sent: Record<string, unknown>[] = [];
    const embedWindow = { postMessage: (m: Record<string, unknown>) => sent.push(m) };
    const pool = embed.createPool({ query: mac });
    const fwd = embed.createForwarder({ pool, grant, role, frame: { contentWindow: embedWindow } });
    const call = (data: Record<string, unknown>, source: unknown = embedWindow) =>
      fwd.handle({ source, data: { cs: "embed", v: 1, ...data } });
    return { embed, mac, sent, fwd, call, embedWindow, pool };
  }

  it("answers a granted query and cuts the rows to the granted fields", async () => {
    const h = harness({ ...GRANT, fields: { queryContext: ["status"] } });
    h.call({ t: "call", id: 1, verb: "queryContext", args: { limit: 5 } });
    await flush();
    const result = h.sent[0].result as Answer;
    expect(h.sent[0]).toMatchObject({ t: "result", id: 1, ok: true });
    expect(Object.keys(result.rows[0].fields)).toEqual(["status"]);
    expect("title" in result.rows[0]).toBe(false);
    expect(h.mac.mock.calls[0][1]).toMatchObject({ select: ["status"], limit: 5 });
  });

  it.each([
    ["an undeclared verb", { verb: "sendAsHuman", args: { to: "x", text: "y" } }],
    ["an action verb", { verb: "openWorkspace", args: {} }],
    ["an undeclared query", { verb: "inbox", args: {} }],
    ["a field outside the grant in select", { verb: "queryContext", args: { select: ["owner"] } }],
    [
      "a field outside the grant in match",
      { verb: "queryContext", args: { match: { owner: "x" } } },
    ],
    [
      "a field outside the grant in filters",
      { verb: "queryContext", args: { filters: [{ key: "owner", op: "eq", value: "x" }] } },
    ],
    ["a field outside the grant in sort", { verb: "queryContext", args: { sort: ["-owner"] } }],
    ["a field outside the grant in groupBy", { verb: "queryContext", args: { groupBy: "owner" } }],
  ])("denies %s with not_declared and asks the Mac nothing", async (_, req) => {
    const h = harness();
    h.call({ t: "call", id: 1, ...req });
    await flush();
    expect(h.sent[0]).toMatchObject({
      t: "result",
      id: 1,
      ok: false,
      error: { code: "not_declared" },
    });
    expect(h.mac).not.toHaveBeenCalled();
  });

  it("refuses prototype keys anywhere in the arguments", async () => {
    const h = harness();
    const hostile = JSON.parse('{"match": {"__proto__": {"status": "done"}}, "limit": 5}');
    h.call({ t: "call", id: 1, verb: "queryContext", args: hostile });
    h.call({
      t: "call",
      id: 2,
      verb: "queryContext",
      args: JSON.parse('{"where": {"constructor": 1}}'),
    });
    await flush();
    expect(h.sent[0]).toMatchObject({ ok: false, error: { code: "invalid_args" } });
    expect(h.sent[1]).toMatchObject({ ok: false });
    expect(h.mac).not.toHaveBeenCalled();
    expect(({} as Record<string, unknown>).status).toBeUndefined();
  });

  it("openFile is unavailable, not silently dropped", async () => {
    const h = harness();
    h.call({ t: "call", id: 3, verb: "openFile", args: { path: "a" } });
    await flush();
    expect(h.sent[0]).toMatchObject({ ok: false, error: { code: "unavailable" } });
  });

  it("ignores a message that did not come from its frame", async () => {
    const h = harness();
    const handled = h.call(
      { t: "call", id: 1, verb: "queryContext", args: {} },
      { postMessage: vi.fn() },
    );
    await flush();
    expect(handled).toBe(false);
    expect(h.sent).toEqual([]);
    expect(h.mac).not.toHaveBeenCalled();
  });

  it("ignores messages that are not embed envelopes or are oversized", async () => {
    const h = harness();
    h.fwd.handle({ source: h.embedWindow, data: { cs: "live", v: 1, op: "query" } });
    h.fwd.handle({ source: h.embedWindow, data: "hello" });
    h.call({
      t: "call",
      id: 1,
      verb: "queryContext",
      args: { match: { status: "x".repeat(20_000) } },
    });
    h.call({ t: "call", id: "1", verb: "queryContext", args: {} });
    await flush();
    expect(h.sent).toEqual([]);
    expect(h.mac).not.toHaveBeenCalled();
  });

  it("takes grant, token, role and embed fields from nobody but its own page", async () => {
    const h = harness({ ...GRANT, queries: [], streams: ["inbox"] } as never);
    h.call({
      t: "call",
      id: 1,
      verb: "inbox",
      args: {},
      grant: { v: 1, queries: ["inbox"], fields: {} },
      token: "t",
      role: "owner",
      embedId: "other",
      scope: "all",
    });
    await flush();
    expect(h.sent[0]).toMatchObject({ ok: false, error: { code: "not_declared" } });
    expect(h.mac).not.toHaveBeenCalled();
  });

  it.each([
    ["no grant", null],
    ["a malformed grant", "{not json"],
    ["a grant of the wrong version", { ...GRANT, v: 2 }],
    ["an empty grant", { v: 1, queries: [], streams: [], fields: {} }],
    ["a grant with a non-list field entry", { ...GRANT, fields: { queryContext: "status" } }],
  ])("denies everything with %s", async (_, grant) => {
    const h = harness(grant);
    h.call({ t: "call", id: 1, verb: "queryContext", args: {} });
    await flush();
    expect(h.sent[0]).toMatchObject({ ok: false, error: { code: "not_declared" } });
    expect(h.mac).not.toHaveBeenCalled();
  });

  it("rate limits one embed at 10 a minute and lets it back in after the window", async () => {
    const h = harness();
    for (let i = 1; i <= 11; i++) {
      h.call({ t: "call", id: i, verb: "queryContext", args: {} });
      await flush();
      if (i % 2 === 0) await vi.advanceTimersByTimeAsync(1);
    }
    const last = h.sent[10];
    expect(last).toMatchObject({ id: 11, ok: false, error: { code: "rate_limited" } });
    expect(h.sent.slice(0, 10).every((m) => m.ok === true)).toBe(true);
    await vi.advanceTimersByTimeAsync(61_000);
    h.call({ t: "call", id: 12, verb: "queryContext", args: {} });
    await flush();
    expect(h.sent[11]).toMatchObject({ id: 12, ok: true });
  });

  it("holds every embed on a page to one shared budget", async () => {
    const embed = loadEmbed();
    const mac = macQuery(() => DATA_A);
    const pool = embed.createPool({ query: mac });
    const pairs = [0, 1, 2, 3].map(() => nestedPair(embed, GRANT, { query: mac }, pool));
    const sdks = pairs.map((p) => embed.createSdk(p.transport));
    let ok = 0;
    let limited = 0;
    for (let i = 0; i < 40; i++) {
      await Promise.resolve(sdks[i % 4].queryContext({})).then(
        () => ok++,
        (e: { code: string }) => e.code === "rate_limited" && limited++,
      );
      await vi.advanceTimersByTimeAsync(1);
    }
    expect(ok).toBe(30);
    expect(limited).toBe(10);
    expect(mac).toHaveBeenCalledTimes(30);
  });

  it("shares one poll between identical subscriptions and pauses a hidden embed", async () => {
    const embed = loadEmbed();
    const mac = macQuery(() => DATA_A);
    const pool = embed.createPool({ query: mac });
    const a = nestedPair(embed, GRANT, { query: mac }, pool);
    const b = nestedPair(embed, GRANT, { query: mac }, pool);
    const got: unknown[] = [];
    const sdkA = embed.createSdk(a.transport);
    const sdkB = embed.createSdk(b.transport);
    sdkA.subscribeContext({ select: ["status"] }, (r: unknown) => got.push(["a", r]));
    sdkB.subscribeContext({ select: ["status"] }, (r: unknown) => got.push(["b", r]));
    await flush();
    expect(mac).toHaveBeenCalledTimes(1);
    expect(got.map((g) => (g as string[])[0]).sort()).toEqual(["a", "b"]);
    expect(pool._stats().polls).toBe(1);
    await vi.advanceTimersByTimeAsync(15_000);
    await flush();
    expect(mac).toHaveBeenCalledTimes(2);
    a.fwd.setVisible(false);
    b.fwd.setVisible(false);
    await vi.advanceTimersByTimeAsync(60_000);
    await flush();
    expect(mac).toHaveBeenCalledTimes(2);
    a.fwd.setVisible(true);
    await flush();
    expect(mac).toHaveBeenCalledTimes(3);
  });

  it("caps subscriptions per embed and stops polling once all are gone", async () => {
    const embed = loadEmbed();
    const mac = macQuery(() => DATA_A);
    const pair = nestedPair(embed, GRANT, { query: mac });
    const sdk = embed.createSdk(pair.transport);
    const errors: string[] = [];
    const stops = [1, 2, 3, 4, 5].map((n) =>
      sdk.subscribeContext(
        { limit: n },
        (_: unknown, e: { code: string } | null) => e && errors.push(e.code),
      ),
    );
    await flush();
    expect(errors).toEqual(["rate_limited"]);
    (stops as (() => void)[]).forEach((stop) => stop());
    await flush();
    expect(pair.pool._stats().polls).toBe(0);
    pair.fwd.destroy();
  });

  describe("recurring reads are charged to the embed (review r1)", () => {
    const subscribeDistinct = (
      sdk: ReturnType<CsEmbed["createSdk"]>,
      n: number,
      log: unknown[][],
    ) =>
      Array.from({ length: n }, (_, i) =>
        sdk.subscribeContext({ limit: i + 1 }, (res: unknown, err: { code: string } | null) =>
          log.push([i, res ? "ok" : err?.code]),
        ),
      );

    it("4 distinct subscriptions plus calls over a minute stay at 10 reads for that embed, and the rest is said aloud", async () => {
      const embed = loadEmbed();
      const mac = macQuery(() => DATA_A);
      const pair = nestedPair(embed, GRANT, { query: mac });
      const sdk = embed.createSdk(pair.transport);
      const log: unknown[][] = [];
      subscribeDistinct(sdk, 4, log);
      await flush();
      for (let i = 0; i < 6; i++) {
        await sdk.queryContext({});
        await vi.advanceTimersByTimeAsync(1_000);
      }
      await vi.advanceTimersByTimeAsync(53_000); // t = 59 s
      await flush();
      expect(mac).toHaveBeenCalledTimes(10);
      // The refused ticks reached the embed as rate_limited, never silently.
      const limited = log.filter((l) => l[1] === "rate_limited");
      expect(limited.length).toBeGreaterThanOrEqual(8);
      // A minute later the allowance is back and polling resumes.
      await vi.advanceTimersByTimeAsync(20_000);
      await flush();
      expect(mac.mock.calls.length).toBeGreaterThan(10);
      expect(mac.mock.calls.length).toBeLessThanOrEqual(20);
      pair.fwd.destroy();
    });

    it("an embed that spent its own allowance does not starve another embed", async () => {
      const embed = loadEmbed();
      const mac = macQuery(() => DATA_A);
      const pool = embed.createPool({ query: mac });
      const a = nestedPair(embed, GRANT, { query: mac }, pool);
      const b = nestedPair(embed, GRANT, { query: mac }, pool);
      const sdkA = embed.createSdk(a.transport);
      const sdkB = embed.createSdk(b.transport);
      const logA: unknown[][] = [];
      const logB: unknown[][] = [];
      subscribeDistinct(sdkA, 4, logA);
      for (let i = 0; i < 6; i++) await sdkA.queryContext({ limit: 100 + i });
      sdkB.subscribeContext({ limit: 999 }, (res: unknown, err: { code: string } | null) =>
        logB.push([res ? "ok" : err?.code]),
      );
      await vi.advanceTimersByTimeAsync(59_000);
      await flush();
      expect(logB.every((l) => l[0] === "ok")).toBe(true);
      expect(logB.length).toBeGreaterThanOrEqual(4); // t = 0, 15, 30, 45
      expect(mac.mock.calls.length).toBeLessThanOrEqual(10 + 4 + 1);
    });

    it("identical subscriptions share one upstream read, and each embed pays for its own tick", async () => {
      const embed = loadEmbed();
      const mac = macQuery(() => DATA_A);
      const pool = embed.createPool({ query: mac });
      const a = nestedPair(embed, GRANT, { query: mac }, pool);
      const b = nestedPair(embed, GRANT, { query: mac }, pool);
      const sdkA = embed.createSdk(a.transport);
      const sdkB = embed.createSdk(b.transport);
      const logA: unknown[][] = [];
      const logB: unknown[][] = [];
      sdkA.subscribeContext({ limit: 7 }, (res: unknown, err: { code: string } | null) =>
        logA.push([res ? "ok" : err?.code]),
      );
      sdkB.subscribeContext({ limit: 7 }, (res: unknown, err: { code: string } | null) =>
        logB.push([res ? "ok" : err?.code]),
      );
      await flush();
      expect(mac).toHaveBeenCalledTimes(1); // one upstream read served both
      // B burns the rest of its own allowance on one-shot reads (1 + 9 = 10).
      for (let i = 0; i < 9; i++) await sdkB.queryContext({ limit: 50 + i });
      await vi.advanceTimersByTimeAsync(15_000);
      await flush();
      expect(logB.at(-1)).toEqual(["rate_limited"]); // B could not pay for the tick
      expect(logA.at(-1)).toEqual(["ok"]); // A paid and was served
      expect(logA.length).toBe(2);
    });

    it("four busy embeds on one page stay inside the page's 30 a minute", async () => {
      const embed = loadEmbed();
      const mac = macQuery(() => DATA_A);
      const pool = embed.createPool({ query: mac });
      const pairs = [0, 1, 2, 3].map(() => nestedPair(embed, GRANT, { query: mac }, pool));
      pairs.forEach((p) => subscribeDistinct(embed.createSdk(p.transport), 4, []));
      await vi.advanceTimersByTimeAsync(59_000);
      await flush();
      expect(mac.mock.calls.length).toBeLessThanOrEqual(30);
    });
  });

  it("maps the relay's words onto the local ones", async () => {
    const embed = loadEmbed();
    const words: Record<string, string> = {
      mac_offline: "mac_offline",
      rate_limited: "rate_limited",
      no_answer: "no_answer",
      query_not_declared: "not_declared",
      not_approved: "not_declared",
      arg_not_allowed: "arg_not_allowed",
      something_else: "failed",
    };
    for (const [relay, local] of Object.entries(words)) {
      const pool = embed.createPool({ query: () => Promise.reject(new Error(relay)) });
      await expect(pool.ask("queryContext", {})).rejects.toMatchObject({ code: local });
    }
  });

  it("filters inbox optional fields the grant does not name", async () => {
    const embed = loadEmbed();
    const grant = {
      v: 1,
      run: "owner",
      queries: ["inbox"],
      streams: [],
      fields: { inbox: ["text"] },
    };
    const message = {
      messageId: 1,
      text: "hi",
      to: "human",
      toName: "h",
      attachments: [{ sha256: "x" }],
      summary: "s",
    };
    const pool = embed.createPool({ query: async () => ({ messages: [message] }) });
    const sdk = embed.createSdk(embed.directTransport(pool, grant, () => "owner"));
    const res = (await sdk.inbox({ scope: "to-human" })) as { messages: Record<string, unknown>[] };
    expect(res.messages[0]).toEqual({ messageId: 1, text: "hi", summary: "s" });
  });
});

describe("who the scripts run for (live.embeds.run)", () => {
  beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] }));
  afterEach(() => vi.useRealTimers());

  function asking(run: unknown, role: string | null) {
    const embed = loadEmbed();
    const grant = run === undefined ? { ...GRANT, run: undefined } : { ...GRANT, run };
    const mac = macQuery(() => DATA_A);
    const sent: Record<string, unknown>[] = [];
    const embedWindow = { postMessage: (m: Record<string, unknown>) => sent.push(m) };
    const fwd = embed.createForwarder({
      pool: embed.createPool({ query: mac }),
      grant,
      role: () => role,
      frame: { contentWindow: embedWindow },
    });
    const ask = async (verb: string, args: unknown = {}) => {
      sent.length = 0;
      fwd.handle({
        source: embedWindow,
        data: { cs: "embed", v: 1, t: "call", id: 1, verb, args },
      });
      await flush();
      return sent[0];
    };
    return { embed, grant, mac, ask, fwd, sent, embedWindow };
  }

  it("defaults to none: a grant without run never runs, for anyone", async () => {
    for (const role of ["owner", "viewer", null]) {
      const t = asking(undefined, role);
      expect(t.embed.parseGrant(t.grant)).toMatchObject({ run: "none" });
      expect(t.embed.mayRun(t.grant, role)).toBe(false);
      expect(await t.ask("queryContext")).toMatchObject({
        ok: false,
        error: { code: "not_declared" },
      });
      expect(t.mac).not.toHaveBeenCalled();
    }
  });

  it.each([
    ["none", "owner", false],
    ["none", "viewer", false],
    ["owner", "owner", true],
    ["owner", "viewer", false],
    ["owner", null, false],
    ["everyone", "owner", true],
    ["everyone", "viewer", true],
    ["everyone", null, false],
    ["everyone", "admin", false],
  ])("run %s with a %s session: scripts and bridge %s", async (run, role, allowed) => {
    const t = asking(run, role);
    expect(t.embed.mayRun(t.grant, role)).toBe(allowed);
    const reply = await t.ask("queryContext");
    expect(reply.ok).toBe(allowed);
    expect(t.mac).toHaveBeenCalledTimes(allowed ? 1 : 0);
    if (!allowed) expect(reply).toMatchObject({ error: { code: "not_declared" } });
  });

  it("no one acts through the pipe: every action verb is refused, viewers and owners, under every run", async () => {
    for (const run of ["owner", "everyone"]) {
      for (const role of ["owner", "viewer"]) {
        const t = asking(run, role);
        for (const verb of [
          "sendAsHuman",
          "replyAsHuman",
          "postMessage",
          "markRead",
          "archive",
          "setField",
          "setStatus",
          "openWorkspace",
          "runPrompt",
          "sendToSession",
          "routineRefresh",
          "act",
          "invoke",
        ]) {
          const reply = await t.ask(verb, { to: "x", text: "y" });
          expect(reply, `${run}/${role}/${verb}`).toMatchObject({
            ok: false,
            error: { code: "not_declared" },
          });
        }
        expect(t.mac).not.toHaveBeenCalled();
      }
    }
  });

  it("reads the role on every call, so a session that changes role loses the pipe at once", async () => {
    const embed = loadEmbed();
    let role: string | null = "owner";
    const h = nestedPair(embed, GRANT, { query: macQuery(() => DATA_A) }, undefined, () => role);
    const sdk = embed.createSdk(h.transport);
    await expect(sdk.queryContext({})).resolves.toBeTruthy();
    role = "viewer";
    await expect(sdk.queryContext({})).rejects.toMatchObject({ code: "not_declared" });
  });

  it("a malformed run or grant denies, and hello advertises nothing to a session that may not run", async () => {
    const embed = loadEmbed();
    for (const bad of ["viewers", "OWNER", 1, null, {}]) {
      expect(embed.parseGrant({ ...GRANT, run: bad })).toBeNull();
    }
    const denied = nestedPair(
      embed,
      GRANT,
      { query: macQuery(() => DATA_A) },
      undefined,
      () => "viewer",
    );
    await flush();
    expect(denied.transport.caps()).toEqual([]);
    const allowed = nestedPair(embed, GRANT, { query: macQuery(() => DATA_A) });
    await flush();
    expect(allowed.transport.caps()).toContain("queryContext");
  });

  it("the direct transport honours the role too", async () => {
    const embed = loadEmbed();
    const sdk = embed.createSdk(
      embed.directTransport(
        embed.createPool({ query: macQuery(() => DATA_A) }),
        { ...GRANT, run: "owner" },
        () => "viewer",
      ),
    );
    await expect(sdk.queryContext({})).rejects.toMatchObject({ code: "not_declared" });
  });

  it("a page asked to build the frame builds it only when mayRun says so", () => {
    const embed = loadEmbed();
    expect(embed.mayRun({ ...GRANT, run: "none" }, "owner")).toBe(false);
    expect(embed.mayRun(undefined, "owner")).toBe(false);
    expect(embed.mayRun("not json", "owner")).toBe(false);
    expect(embed.mayRun(JSON.stringify(GRANT), "owner")).toBe(true);
  });
});

describe("the vendored file", () => {
  it("passes publish_dashboard's bridge-call scan, line by line (the pattern payload.rs refuses)", () => {
    const pattern = /window\.codesense|\bcodesense\.act\b|\bcodesense\.\w+\s*\(/;
    const hits = EMBED_JS.split("\n")
      .map((l, i) => [i + 1, l] as const)
      .filter(([, l]) => pattern.test(l));
    expect(hits).toEqual([]);
    expect(EMBED_JS.startsWith("// cs-embed 1\n")).toBe(true);
  });

  it("makes no network call and holds no token", () => {
    for (const banned of [
      "fetch(",
      "XMLHttpRequest",
      "WebSocket",
      "EventSource",
      "localStorage",
      "document.cookie",
      "parent.parent",
      "window.top",
    ]) {
      expect(EMBED_JS, banned).not.toContain(banned);
    }
  });
});

describe("the sandboxed document", () => {
  it("carries a closed CSP, the SDK, then the body, and the SDK source parses", () => {
    const embed = loadEmbed();
    const doc = embed.buildSrcdoc('<p id="x">hi</p>');
    expect(doc).toContain("default-src 'none'");
    expect(doc).toContain("img-src data:");
    expect(doc).not.toContain("connect-src");
    expect(doc.indexOf('window["codesense"]=')).toBeLessThan(doc.indexOf('<p id="x">'));
    expect(doc).not.toMatch(/<\/script>[\s\S]*<\/script>[\s\S]*<\/body>/);
    const script = doc.match(/<script>([\s\S]*?)<\/script>/)![1];
    expect(() => new Function("window", script)).not.toThrow();
  });

  it("boots in a stand-in window and exposes the SDK", () => {
    const embed = loadEmbed();
    const script = embed.buildSrcdoc("").match(/<script>([\s\S]*?)<\/script>/)![1];
    const posted: unknown[] = [];
    const win = {
      parent: { postMessage: (m: unknown) => posted.push(m) },
      addEventListener: () => {},
    } as Record<string, unknown>;
    new Function("window", script)(win);
    expect(typeof (win.codesense as Record<string, unknown>).queryContext).toBe("function");
    expect(posted).toEqual([{ t: "hello", cs: "embed", v: 1 }]);
  });
});
