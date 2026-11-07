// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  CORE_JS,
  HOSTS,
  spliced,
  // @ts-expect-error -- a plain Node ESM script without type declarations.
} from "../../../scripts/gen-dashboard-kit-core.mjs";

// SV-CDE-86: the dashboard patterns (K1 boot, K2 paced, K3 honest values, K4 totals, K7 cachedQuery, K8 section,
// K9 budget, K10 keyedRegion) are ONE source spliced into both hosts. Every case below runs against the in-app kit
// (kit.js over a bridge) and the hosted toolkit (cs-live.js over the share shell's messages) on a virtual clock.

const KIT_JS = readFileSync(HOSTS[0], "utf8");
const LIVE_JS = readFileSync(HOSTS[1], "utf8");

type Pending = {
  name: string;
  args: Record<string, unknown>;
  resolve: (answer: unknown) => void;
  reject: (message: string) => void;
};
type Core = {
  coreVersion: number;
  boot: (o: Record<string, unknown>) => {
    host: string;
    state: {
      ready: boolean;
      error: string | null;
      data: Record<string, { state: string; value: unknown }>;
    };
    stop: () => void;
  };
  budget: (o?: Record<string, unknown>) => {
    config: () => { inFlight: number; minGapMs: number; bytesPerMinute: number; timeoutMs: number };
    stats: () => {
      calls: number;
      inFlight: number;
      queued: number;
      waiting: boolean;
      busy: boolean;
    };
    state: () => string;
    onChange: (cb: (stats: unknown) => void) => () => void;
  };
  query: (name: string, args?: Record<string, unknown>) => Promise<unknown>;
  queryAll: (
    name: string,
    args: Record<string, unknown>,
    opts?: Record<string, unknown>,
  ) => Promise<unknown>;
  paced: (
    q: unknown,
    o?: Record<string, unknown>,
  ) => {
    request: (args?: Record<string, unknown>, o?: Record<string, unknown>) => Promise<unknown>;
    observe: (el: Element, args?: Record<string, unknown>) => void;
    refresh: () => void;
    stop: () => void;
  };
  value: (v: unknown, o?: Record<string, unknown>) => HTMLElement;
  valueState: (v: unknown, o?: Record<string, unknown>) => { state: string; text: string };
  errorText: (err: unknown) => string;
  errorKind: (err: unknown) => string;
  errorCodes: () => Record<string, string>;
  stateBanner: (o: Record<string, unknown>) => HTMLElement | null;
  partial: (known: number, total: number, noun?: string) => string;
  batchTotals: (
    ids: string[] | (() => string[]),
    o?: Record<string, unknown>,
  ) => {
    pull: (o?: { all?: boolean; ids?: string[] }) => Promise<void>;
    nextDelay: () => number | null;
    get: (
      id: string,
    ) =>
      | ({ kind: string; totals?: Record<string, unknown>; computedAtMs?: number | null } & Record<
          string,
          unknown
        >)
      | undefined;
    markDirty: (ids: string[]) => void;
    needs: (id: string) => boolean;
  };
  visibleTotals: (
    c: Element,
    o: Record<string, unknown>,
  ) => {
    ids: () => string[];
    reader: { pull: (o?: Record<string, unknown>) => Promise<void>; slowIds: () => string[] };
    changed: (ids: string[]) => void;
    get: (id: string) => { kind: string; computedAtMs?: number | null } | undefined;
    refresh: (o?: { refused?: boolean }) => Promise<void>;
    stop: () => void;
  };
  cachedQuery: (
    n: string,
    a: Record<string, unknown>,
    o: Record<string, unknown>,
  ) => {
    peek: () => { value: unknown; ageMs: number | null; state: string; error: Error | null };
    refresh: () => Promise<unknown>;
    ready: Promise<unknown>;
  };
  section: (
    el: HTMLElement,
    o: Record<string, unknown>,
  ) => {
    refresh: () => Promise<unknown>;
    cancel: () => void;
    stop: () => void;
  };
  closeScope: (scope: unknown) => void;
  dataChanged: (ev: {
    ids?: string[];
    keys?: (string | { key: string; scope?: unknown })[];
  }) => void;
  live?: (render: () => unknown, opts: Record<string, unknown>) => () => void;
  watch?: (
    stream: string,
    args: Record<string, unknown>,
    handlers: Record<string, unknown>,
  ) => { stop: () => void };
  keyedRegion: (
    el: HTMLElement,
    scope?: unknown,
  ) => {
    setScope: (s: unknown) => void;
    paint: (items: unknown[], o: Record<string, unknown>, scope?: unknown) => boolean;
    flush: () => void;
    stop: () => void;
  };
};
type Loaded = {
  core: Core;
  calls: Pending[];
  serve: (name: string, answer: unknown) => void;
  fail: (name: string, m: string) => void;
};

const win = window as unknown as Record<string, unknown>;

// The host's visibility callbacks (the app bridge's onVisibility) and every message the hosted page posted.
const visibilityCbs: ((shown: boolean) => void)[] = [];
let hubChange: ((snap: unknown) => void) | undefined;
let postedOps: string[] = [];
// Hide or show the page the way a tab switch does: the document, the window and the host's own callback.
function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", { value: hidden, configurable: true });
  Object.defineProperty(document, "visibilityState", {
    value: hidden ? "hidden" : "visible",
    configurable: true,
  });
  visibilityCbs.forEach((cb) => cb(!hidden));
  document.dispatchEvent(new Event("visibilitychange"));
  window.dispatchEvent(new Event(hidden ? "blur" : "focus"));
  if (!hidden) window.dispatchEvent(new Event("pageshow"));
}

const storedKeys = () =>
  Array.from(
    { length: window.localStorage.length },
    (_, i) => window.localStorage.key(i) as string,
  );

function resetPage() {
  delete win.__csKitCore;
  delete win.CS;
  delete win.codesense;
  visibilityCbs.length = 0;
  window.localStorage.clear();
  document.body.replaceChildren();
}

// In-app: a bridge whose invoke() waits for the test to answer.
function loadApp(calls: Pending[]): Core {
  win.codesense = {
    has: () => true,
    subscribe: (_topic: string, o: { onChange: (snap: unknown) => void }) => {
      hubChange = o.onChange;
      return Promise.resolve();
    },
    unsubscribe: () => {},
    onVisibility: (cb: (shown: boolean) => void) => {
      visibilityCbs.push(cb);
      cb(true);
      return () => {
        visibilityCbs.splice(visibilityCbs.indexOf(cb), 1);
      };
    },
    invoke: (name: string, args: Record<string, unknown>) =>
      new Promise((resolve, reject) =>
        calls.push({ name, args, resolve, reject: (m) => reject(new Error(m)) }),
      ),
  };
  new Function(KIT_JS)();
  return (win.codesense as { kit: Core }).kit;
}

// Hosted: the share shell's messages. The page posts a query to itself; the test answers as the viewer would.
// The share shell's window, and whether it has said hello yet (a test can hold the hello back).
let shellParent: object = {};
let withHello = true;
function loadHosted(calls: Pending[]): Core {
  const parent = {};
  shellParent = parent;
  Object.defineProperty(window, "parent", { value: parent, configurable: true });
  postedOps = [];
  vi.spyOn(window, "postMessage").mockImplementation(((msg: Record<string, unknown>) => {
    postedOps.push(String(msg.op));
    if (msg.op !== "query") return;
    const reply = (extra: Record<string, unknown>) =>
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { cs: "live", v: 1, op: "answer", id: msg.id, ...extra },
          source: parent as Window,
        }),
      );
    calls.push({
      name: msg.query as string,
      args: msg.args as Record<string, unknown>,
      resolve: (answer) => reply({ ok: true, answer }),
      reject: (m) => reply({ ok: false, error: m }),
    });
  }) as typeof window.postMessage);
  const CS = new Function(`${LIVE_JS}\nreturn CS;`)() as Core;
  if (withHello)
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { cs: "live", v: 1, op: "hello", role: "viewer", control: false },
        source: parent as Window,
      }),
    );
  return CS;
}

const HOST_LOADERS = { app: loadApp, hosted: loadHosted } as const;
function load(host: keyof typeof HOST_LOADERS): Loaded {
  const calls: Pending[] = [];
  const core = HOST_LOADERS[host](calls);
  const take = (name: string) => {
    const i = calls.findIndex((c) => c.name === name);
    if (i < 0) throw new Error(`no pending ${name}`);
    return calls.splice(i, 1)[0];
  };
  return { core, calls, serve: (n, a) => take(n).resolve(a), fail: (n, m) => take(n).reject(m) };
}

const tick = (ms: number) => vi.advanceTimersByTimeAsync(ms);

// IntersectionObserver the test drives.
type Watcher = { cb: (entries: Partial<IntersectionObserverEntry>[]) => void; seen: Set<Element> };
let watchers: Watcher[] = [];
class FakeObserver {
  w: Watcher;
  constructor(cb: Watcher["cb"]) {
    this.w = { cb, seen: new Set() };
    watchers.push(this.w);
  }
  observe(el: Element) {
    this.w.seen.add(el);
  }
  unobserve(el: Element) {
    this.w.seen.delete(el);
  }
  disconnect() {
    this.w.seen.clear();
  }
}
function see(els: Element[], isIntersecting: boolean) {
  watchers.forEach((w) => {
    const hit = els.filter((e) => w.seen.has(e));
    if (hit.length) w.cb(hit.map((target) => ({ target, isIntersecting })));
  });
}

const story = (_id: string, extra: Record<string, unknown> = {}) => ({
  totals: { costUsd: 1, sessions: 2, ...extra },
  computedAtMs: 111,
});
const answerFor = (ids: string[], make = story) => ({
  stories: Object.fromEntries(ids.map((id) => [id, make(id)])),
  reasons: [],
});

const addedListeners: [EventTarget, string, EventListenerOrEventListenerObject, unknown][] = [];
beforeEach(() => {
  for (const target of [window, document] as EventTarget[]) {
    const original = target.addEventListener.bind(target);
    vi.spyOn(target, "addEventListener").mockImplementation(((
      type: string,
      handler: EventListenerOrEventListenerObject,
      options?: unknown,
    ) => {
      addedListeners.push([target, type, handler, options]);
      original(type, handler, options as AddEventListenerOptions);
    }) as never);
  }
  vi.useFakeTimers({ now: new Date("2026-10-30T10:00:00Z") });
  resetPage();
  watchers = [];
  vi.stubGlobal("IntersectionObserver", FakeObserver);
  if (!(globalThis as { CSS?: { escape?: unknown } }).CSS?.escape)
    vi.stubGlobal("CSS", { escape: (s: string) => s });
});
afterEach(() => {
  addedListeners
    .splice(0)
    .forEach(([target, type, handler, options]) =>
      target.removeEventListener(type, handler, options as EventListenerOptions),
    );
  delete (document as unknown as Record<string, unknown>).hidden;
  delete (document as unknown as Record<string, unknown>).visibilityState;
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("the shipped sources", () => {
  it("both hosts carry the one core source, byte for byte", () => {
    const core = readFileSync(CORE_JS, "utf8");
    for (const file of HOSTS)
      expect(spliced(readFileSync(file, "utf8"), core)).toBe(readFileSync(file, "utf8"));
  });
  it("the hosted toolkit header and Rust constant name the same toolkit version", () => {
    expect(LIVE_JS.startsWith("// cs-live 4\n")).toBe(true);
    expect(
      readFileSync(join(process.cwd(), "src-tauri/src/dashboard_publish/create.rs"), "utf8"),
    ).toContain("TOOLKIT_VERSION: u32 = 4;");
  });
});

describe("the kit's code table", () => {
  it("every code in LIVE_ERRORS (the R23 list) is classified, so a new hosted code fails here until it is", () => {
    const rust = readFileSync(
      join(process.cwd(), "src-tauri/src/mcp/live_data_authoring.rs"),
      "utf8",
    );
    const block = /const LIVE_ERRORS[^=]*=\s*&\[([\s\S]*?)\n\];/.exec(rust)![1];
    const codes = [...block.matchAll(/\(\s*"([a-z0-9_]+)"\s*,/g)].map((m) => m[1]);
    const entries = block.split("\n").filter((line) => /^\s*\("/.test(line)).length;
    expect(codes.length).toBeGreaterThan(10);
    expect(codes).toHaveLength(entries);
    const table = load("hosted").core.errorCodes();
    for (const code of codes)
      expect(["refusal", "transient", "other"], code).toContain(table[code]);
    for (const code of ["arg_not_allowed", "query_not_declared", "query_invalid", "access_ended"])
      expect(table[code], code).toBe("refusal");
    for (const code of ["mac_offline", "mac_timeout", "mac_busy", "rate_limited", "busy"])
      expect(table[code], code).toBe("transient");
    for (const code of ["answer_failed", "answer_too_large"])
      expect(table[code], code).toBe("other");
  });
});

describe.each(["app", "hosted"] as const)("dashboard kit core, %s host", (host) => {
  it("exposes the same API and says which host it is", () => {
    const { core } = load(host);
    for (const name of [
      "boot",
      "paced",
      "budget",
      "value",
      "valueState",
      "errorText",
      "stateBanner",
      "partial",
      "batchTotals",
      "visibleTotals",
      "cachedQuery",
      "section",
      "keyedRegion",
    ])
      expect(typeof (core as unknown as Record<string, unknown>)[name], name).toBe("function");
    expect(core.coreVersion).toBe(4);
    expect(core.boot({}).host).toBe(host);
  });

  describe("K9 budget", () => {
    it("a watcher that calls budget().stats() inside onChange does not recurse, and the page keeps working", async () => {
      const { core, calls, serve } = load(host);
      const seen: number[] = [];
      const off = core.budget().onChange(() => {
        seen.push(core.budget().stats().inFlight);
        core.budget({ inFlight: 2 });
      });
      const first = core.cachedQuery("a", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["a"]);
      expect(seen.length).toBeGreaterThan(0);
      serve("a", 1);
      await tick(0);
      expect(first.peek().state).toBe("fresh");
      // A changed setting still pumps, from inside a watcher too, without recursing.
      core.budget({ inFlight: 1 });
      const second = core.cachedQuery("b", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["b"]);
      serve("b", 2);
      await tick(0);
      expect(second.peek().state).toBe("fresh");
      off();
    });

    it("never has more than 2 reads in flight, and a queued ask runs when a slot frees", async () => {
      const { core, calls, serve } = load(host);
      const budget = core.budget();
      const handles = ["a", "b", "c", "d", "e"].map((n) =>
        core.cachedQuery(n, {}, { maxAgeMs: 1000, key: "t" }),
      );
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["a", "b"]);
      expect(budget.stats()).toMatchObject({ inFlight: 2, queued: 3, waiting: true });
      serve("a", 1);
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["b", "c"]);
      expect(handles[0].peek().state).toBe("fresh");
      expect(budget.stats().inFlight).toBe(2);
    });

    it("a newer ask in the same slot drops the older one still queued", async () => {
      const { core, calls, serve } = load(host);
      core.budget({ inFlight: 1 });
      const first = core.paced("hold", { key: "hold" }).request({ n: 0 });
      const queuedA = core.paced("x", { key: "slot" }).request({ n: 1 });
      const dropped = expect(queuedA).rejects.toMatchObject({ superseded: true });
      const queuedB = core.paced("x", { key: "slot" }).request({ n: 1 });
      await tick(0);
      serve("hold", {});
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["x"]);
      serve("x", {});
      await Promise.all([first, queuedB, dropped]);
    });

    it("both files on one page share one core, so one budget", async () => {
      const calls: Pending[] = [];
      const kit = loadApp(calls);
      const live = new Function(`${LIVE_JS}\nreturn CS;`)() as Core;
      expect(live.budget).toBe(kit.budget);
      live.cachedQuery("h1", {}, { maxAgeMs: 1000, key: "t" });
      live.cachedQuery("h2", {}, { maxAgeMs: 1000, key: "t" });
      kit.cachedQuery("k1", {}, { maxAgeMs: 1000, key: "t" });
      kit.cachedQuery("k2", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      expect(calls.length).toBe(2);
      expect(kit.budget().stats().queued).toBe(2);
    });

    it("a CS that is already on the page is not replaced by the in-app kit", () => {
      if (host !== "app") return;
      const mine = { mine: true };
      win.CS = mine;
      loadApp([]);
      expect(win.CS).toBe(mine);
    });
  });

  describe("K7 cachedQuery", () => {
    const ask = { maxAgeMs: 60_000, key: "page" };
    it("needs maxAgeMs", () => {
      const { core } = load(host);
      expect(() => core.cachedQuery("q", {}, { key: "page" })).toThrow(/maxAgeMs/);
    });

    it("cold: nothing yet, one read, then fresh; a second ask for the same key shares it", async () => {
      const { core, calls, serve } = load(host);
      const a = core.cachedQuery("q", { n: 1 }, ask);
      const b = core.cachedQuery("q", { n: 1 }, ask);
      expect(b).toBe(a);
      expect(a.peek()).toMatchObject({ value: undefined, state: "none", ageMs: null });
      await tick(0);
      expect(calls.length).toBe(1);
      serve("q", { rows: [1] });
      await a.ready;
      expect(a.peek()).toMatchObject({ value: { rows: [1] }, state: "fresh" });
    });

    // Only the in-app kit registers storage; the published toolkit keeps answers in memory for the page (see the hosted test below).
    it.runIf(host === "app")(
      "warm: a young answer paints at once from storage, then exactly one read revalidates it",
      async () => {
        const first = load(host);
        first.core.cachedQuery("q", {}, ask);
        await tick(0);
        first.serve("q", { rows: [1, 2] });
        await tick(0);
        resetKeepStorage();
        const second = load(host);
        const warm = second.core.cachedQuery("q", {}, ask);
        expect(warm.peek()).toMatchObject({ value: { rows: [1, 2] }, state: "cached" });
        await tick(0);
        expect(second.calls.length).toBe(1);
        second.serve("q", { rows: [1, 2, 3] });
        await warm.ready;
        expect(warm.peek()).toMatchObject({ value: { rows: [1, 2, 3] }, state: "fresh" });
        await tick(5000);
        expect(second.calls.length).toBe(0);
      },
    );

    it.runIf(host === "app")(
      "stale: an old answer shows while one read revalidates it, and a failed read keeps the value",
      async () => {
        const first = load(host);
        first.core.cachedQuery("q", {}, ask);
        await tick(0);
        first.serve("q", "old");
        await tick(61_000);
        resetKeepStorage();
        const second = load(host);
        const handle = second.core.cachedQuery("q", {}, ask);
        expect(handle.peek()).toMatchObject({ value: "old", state: "stale" });
        await tick(0);
        expect(second.calls.length).toBe(1);
        second.fail("q", "no_answer");
        await handle.ready;
        expect(handle.peek()).toMatchObject({ value: "old", state: "stale" });
        expect(handle.peek().error?.message).toBe("no_answer");
      },
    );

    it("never shares an entry between hosts, and works when storage throws", async () => {
      const { core, serve } = load(host);
      core.cachedQuery("q", {}, ask);
      await tick(0);
      serve("q", 1);
      await tick(0);
      expect(storedKeys().some((k) => k.startsWith(`cs-kit:1:${host}:`))).toBe(host === "app");
      expect(
        storedKeys().some((k) => k.startsWith(`cs-kit:1:${host === "app" ? "hosted" : "app"}:`)),
      ).toBe(false);

      resetPage();
      vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
        throw new Error("denied");
      });
      vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
        throw new Error("denied");
      });
      const sandboxed = load(host);
      const handle = sandboxed.core.cachedQuery("q", {}, ask);
      await tick(0);
      sandboxed.serve("q", 2);
      await handle.ready;
      expect(handle.peek()).toMatchObject({ value: 2, state: "fresh" });
    });

    it.runIf(host === "hosted")(
      "hosted keeps answers in memory only: storage stays untouched and a new page starts empty",
      async () => {
        const first = load(host);
        first.core.cachedQuery("q", {}, ask);
        await tick(0);
        first.serve("q", { rows: [1] });
        await tick(0);
        expect(storedKeys().filter((k) => k.startsWith("cs-kit:"))).toEqual([]);
        resetKeepStorage();
        const second = load(host);
        const cold = second.core.cachedQuery("q", {}, ask);
        expect(cold.peek()).toMatchObject({ value: undefined, state: "none" });
        await tick(0);
        expect(second.calls.length).toBe(1);
      },
    );

    it("an old answer cannot overwrite a newer one", async () => {
      const { core, calls } = load(host);
      const handle = core.cachedQuery("q", {}, ask);
      await tick(0);
      const newer = handle.refresh();
      await tick(2000);
      calls[1].resolve("newer");
      await newer;
      calls[0].resolve("older-answer-late");
      await tick(0);
      expect(handle.peek().value).toBe("newer");
    });
  });

  describe("K3 honest values", () => {
    it("a known 0 stays 0 and a missing value is never a 0", () => {
      const { core } = load(host);
      expect(core.valueState(0)).toEqual({ state: "known", text: "0" });
      expect(core.valueState(null, { unavailable: "cost not recorded" })).toEqual({
        state: "unknown",
        text: "Unknown · Cost not recorded",
      });
      expect(core.valueState(undefined)).toEqual({ state: "unknown", text: "Unknown" });
      expect(core.valueState(NaN).state).toBe("unknown");
    });
    it("pending reads Counting…, distinct from loading, unknown and error; a known value wins over pending", () => {
      const { core } = load(host);
      const texts = [
        core.valueState(null, { pending: true }),
        core.valueState(null, { loading: true }),
        core.valueState(null, { error: true }),
        core.valueState(null),
      ].map((s) => s.text);
      expect(new Set(texts).size).toBe(4);
      expect(core.valueState(null, { pending: true }).text).toBe("Counting…");
      expect(core.valueState(7, { pending: true }).state).toBe("known");
    });
    it("stale values say how old; the element carries its state; error offers Retry", () => {
      const { core } = load(host);
      expect(core.valueState(5, { stale: true, ageMs: 4 * 60_000 }).text).toBe("5 · 4 min ago");
      const retry = vi.fn();
      const node = core.value(null, { error: true, onRetry: retry });
      expect(node.getAttribute("data-state")).toBe("error");
      node.querySelector("button")!.click();
      expect(retry).toHaveBeenCalledOnce();
    });
    it("the banner says offline or stale, and nothing otherwise; partial says N of M", () => {
      const { core } = load(host);
      expect(core.stateBanner({})).toBeNull();
      expect(
        core.stateBanner({ offline: true, asOf: Date.now() - 120_000 })!.textContent,
      ).toContain("offline");
      expect(core.stateBanner({ stale: true, asOf: Date.now() - 120_000 })!.textContent).toContain(
        "2 minutes",
      );
      expect(core.partial(3, 9)).toBe("Totals loading · 3 of 9 stories");
      expect(core.partial(9, 9)).toBe("");
    });
  });

  describe("K4 batch totals and K4+ visible totals", () => {
    it("one storyTotals call for the ids, nulls kept, pending followed up with backoff", async () => {
      const { core, calls, serve } = load(host);
      const reader = core.batchTotals(["A", "B"]);
      const pulled = reader.pull();
      await tick(0);
      expect(calls.map((c) => [c.name, c.args.storyIds])).toEqual([["storyTotals", ["A", "B"]]]);
      serve("storyTotals", {
        stories: { A: story("A", { costUsd: null }), B: { pending: true } },
        reasons: [],
      });
      await pulled;
      expect(reader.get("A")).toMatchObject({
        kind: "ready",
        totals: { costUsd: null },
        computedAtMs: 111,
      });
      expect(reader.get("B")).toMatchObject({ kind: "pending" });
      expect(reader.nextDelay()).toBe(2000);
    });

    function rows(n: number) {
      const list = document.createElement("div");
      for (let i = 0; i < n; i++) {
        const row = document.createElement("div");
        row.setAttribute("data-id", `S-${i}`);
        row.className = "row";
        list.append(row);
      }
      document.body.append(list);
      return { list, all: Array.from(list.children) };
    }

    it("a 200-row list asks only for the visible ids, then only the newly visible ones", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(200);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      await tick(500);
      expect(calls.length).toBe(0);
      see(all.slice(0, 20), true);
      await tick(200);
      expect(calls.map((c) => c.args.storyIds)).toEqual([
        all.slice(0, 20).map((r) => r.getAttribute("data-id")),
      ]);
      serve("storyTotals", answerFor(all.slice(0, 20).map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      see(all.slice(0, 20), false);
      see(all.slice(20, 40), true);
      await tick(200);
      expect(calls.length).toBe(1);
      expect(calls[0].args.storyIds).toEqual(
        all.slice(20, 40).map((r) => r.getAttribute("data-id")),
      );
      expect(totals.get("S-5")).toMatchObject({ kind: "ready" });
      expect(totals.get("S-150")).toBeUndefined();
      totals.stop();
    });

    it("a pending id is asked again with backoff while visible, and not once it has left", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(3);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", {
        stories: { "S-0": { pending: true }, "S-1": story("S-1"), "S-2": { pending: true } },
        reasons: [],
      });
      await tick(0);
      see([all[2]], false);
      await tick(2000);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-0"]]);
      serve("storyTotals", answerFor(["S-0"]));
      await tick(0);
      expect(totals.get("S-0")).toMatchObject({ kind: "ready" });
      expect(totals.get("S-2")).toMatchObject({ kind: "pending" });
      await tick(20_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("a pending id is asked again for as long as it is visible: 2, 4, 8, then every 16 s", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(1);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", { stories: { "S-0": { pending: true } }, reasons: [] });
      await tick(0);
      for (const wait of [2000, 4000, 8000, 16000, 16000, 16000]) {
        await tick(wait - 1);
        expect(calls.length).toBe(0);
        await tick(1);
        expect(calls.length).toBe(1);
        serve("storyTotals", { stories: { "S-0": { pending: true } }, reasons: [] });
        await tick(0);
      }
      totals.stop();
    });

    it("a story the engine refused is not asked again, a pending one is", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(2);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", {
        stories: { "S-0": { error: "pin rejected" }, "S-1": { pending: true } },
        reasons: [],
      });
      await tick(0);
      await tick(2000);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-1"]]);
      serve("storyTotals", answerFor(["S-1"]));
      await tick(0);
      await tick(30_000);
      expect(calls.length).toBe(0);
      expect(totals.get("S-0")).toMatchObject({ kind: "error" });
      totals.stop();
    });

    it("an omitted id, a refused story and a kept-but-refused story are not asked again; a busy one is", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(4);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", {
        stories: {
          "S-0": story("S-0"),
          "S-1": { error: "not_retryable" },
          "S-3": { error: "busy" },
        },
        reasons: [],
      });
      await tick(0);
      expect(totals.get("S-2")).toMatchObject({ kind: "error", error: "omitted" });
      await tick(2000);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-3"]]);
      serve("storyTotals", answerFor(["S-3"]));
      await tick(0);
      const refreshed = totals.refresh();
      await tick(0);
      serve("storyTotals", {
        stories: {
          "S-0": { error: "refused" },
          "S-1": { error: "not_retryable" },
          "S-2": { error: "refused" },
          "S-3": story("S-3"),
        },
        reasons: [],
      });
      await refreshed;
      expect(totals.get("S-0")).toMatchObject({ kind: "ready", kept: true });
      await tick(30_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("a thrown retryable ask is asked again with the 2, 4, 8, 16 s backoff", async () => {
      const { core, calls, fail } = load(host);
      const { list, all } = rows(1);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      for (const wait of [2000, 4000, 8000, 16000, 16000]) {
        fail("storyTotals", "mac_timeout");
        await tick(0);
        await tick(wait - 1);
        expect(calls.length).toBe(0);
        await tick(1);
        expect(calls.length).toBe(1);
      }
      totals.stop();
    });

    it("a refused ask is halved to isolate the refused id: bounded asks, the rest kept, none asked again", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(120);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      let asked = 0;
      for (let i = 0; i < 60 && calls.length; i++) {
        const call = calls.shift()!;
        asked++;
        const ids = call.args.storyIds as string[];
        if (ids.includes("S-37")) call.reject("query_invalid");
        else call.resolve(answerFor(ids));
        await tick(0);
      }
      expect(asked).toBeLessThanOrEqual(15);
      expect(totals.get("S-37")).toMatchObject({ kind: "error", error: "refused" });
      expect(totals.get("S-0")).toMatchObject({ kind: "ready" });
      expect(totals.get("S-119")).toMatchObject({ kind: "ready" });
      await tick(60_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("2 refused ids among 120 rows: exactly those 2 are refused and the other 118 get totals", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(120);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      let asked = 0;
      for (let i = 0; i < 80 && calls.length; i++) {
        const call = calls.shift()!;
        asked++;
        const ids = call.args.storyIds as string[];
        if (ids.includes("S-11") || ids.includes("S-97")) call.reject("query_invalid");
        else call.resolve(answerFor(ids));
        await tick(0);
      }
      expect(asked).toBeLessThanOrEqual(30);
      const ids = all.map((r) => r.getAttribute("data-id")!);
      expect(ids.filter((id) => totals.get(id)?.kind === "ready")).toHaveLength(118);
      expect(totals.get("S-11")).toMatchObject({ kind: "error", error: "refused" });
      expect(totals.get("S-97")).toMatchObject({ kind: "error", error: "refused" });
      await tick(60_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("a live event asks exactly the named visible ids: not another unsettled id, and nobody's backoff restarts", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(10);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      const stories = Object.fromEntries(
        all.map((r, i) => {
          const id = r.getAttribute("data-id")!;
          return [id, i === 9 ? { pending: true } : story(id)];
        }),
      );
      serve("storyTotals", { stories, reasons: [] });
      await tick(0);
      core.dataChanged({ ids: ["S-3", "S-404"] });
      await tick(200);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-3"]]);
      serve("storyTotals", answerFor(["S-3"]));
      await tick(0);
      await tick(1800);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-9"]]);
      totals.stop();
    });

    it("a named id is asked at once; one that comes back refreshing leaves the 2 s chase for the 60 s interval", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(10);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", answerFor(all.map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      core.dataChanged({ ids: ["S-3"] });
      await tick(200);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-3"]]);
      serve("storyTotals", {
        stories: { "S-3": { totals: { costUsd: 1 }, computedAtMs: 500, refreshing: true } },
        reasons: [],
      });
      await tick(0);
      await tick(30_000);
      expect(calls.length).toBe(0);
      await tick(30_100);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-3"]]);
      serve("storyTotals", {
        stories: { "S-3": { totals: { costUsd: 2 }, computedAtMs: 999 } },
        reasons: [],
      });
      await tick(0);
      expect(totals.get("S-3")).toMatchObject({
        kind: "ready",
        computedAtMs: 999,
        refreshing: false,
      });
      await tick(300_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("steady events never push an armed chase out: the old failed id is asked on its 2, 4, 8 s schedule", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(10);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      calls.shift()!.resolve(answerFor(all.map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      const t0 = Date.now();
      const askedAt: number[] = [];
      core.dataChanged({ ids: ["S-3"] });
      for (let step = 0; step < 200; step++) {
        if (step > 0 && step % 10 === 0) core.dataChanged({ ids: ["S-4"] });
        await tick(100);
        while (calls.length) {
          const call = calls.shift()!;
          const ids = call.args.storyIds as string[];
          if (ids.includes("S-3")) {
            askedAt.push(Date.now() - t0);
            call.resolve({ stories: { "S-3": { error: "mac_timeout" } }, reasons: [] });
          } else call.resolve(answerFor(ids));
          await tick(0);
        }
      }
      // The event's own ask, then the chase at about 2 s, 6 s and 14 s: events every second never moved it.
      expect(askedAt).toHaveLength(4);
      [200, 2200, 6200, 14_200].forEach((at, i) =>
        expect(Math.abs(askedAt[i] - at)).toBeLessThanOrEqual(300),
      );
      totals.stop();
    });

    it("a named read that fails mac_timeout is asked again at 2 s", async () => {
      const { core, calls, serve, fail } = load(host);
      const { list, all } = rows(10);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", answerFor(all.map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      core.dataChanged({ ids: ["S-3"] });
      await tick(200);
      fail("storyTotals", "mac_timeout");
      await tick(0);
      await tick(1999);
      expect(calls.length).toBe(0);
      await tick(1);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-3"]]);
      totals.stop();
    });

    it("access_ended is known from one ask: 50 rows, 1 ask, none after", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(50);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      let asked = 0;
      for (let i = 0; i < 10 && calls.length; i++) {
        calls.shift()!.reject("access_ended");
        asked++;
        await tick(0);
      }
      expect(asked).toBe(1);
      expect(totals.get("S-3")).toMatchObject({ error: "refused" });
      await tick(60_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("an unpinned storyTotals (arg_not_allowed for every ask): all 3 probes refused, all ids refused, at most 4 asks, one warning", async () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const { core, calls } = load(host);
      const { list, all } = rows(50);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      let asked = 0;
      for (let i = 0; i < 10 && calls.length; i++) {
        calls.shift()!.reject("arg_not_allowed");
        asked++;
        await tick(0);
      }
      expect(asked).toBeLessThanOrEqual(4);
      expect(warn).toHaveBeenCalledTimes(1);
      expect(String(warn.mock.calls[0][0])).toContain("check the page's storyIds pin");
      expect(totals.get("S-3")).toMatchObject({ error: "refused" });
      await tick(60_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("arg_not_allowed for the first id only: the other probes pass, only that id is refused, the rest get totals", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(20);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      for (let i = 0; i < 40 && calls.length; i++) {
        const call = calls.shift()!;
        const ids = call.args.storyIds as string[];
        if (ids.includes("S-0")) call.reject("arg_not_allowed");
        else call.resolve(answerFor(ids));
        await tick(0);
      }
      const ids = all.map((r) => r.getAttribute("data-id")!);
      expect(totals.get("S-0")).toMatchObject({ kind: "error", error: "refused" });
      expect(ids.filter((id) => totals.get(id)?.kind === "ready")).toHaveLength(19);
      totals.stop();
    });

    it("half of 40 rows out of pin, a mix in the probe round: no verb verdict, every in-pin id ready, every out-of-pin id refused", async () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const { core, calls } = load(host);
      const { list, all } = rows(40);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      const out = (id: string) => Number(id.slice(2)) % 2 === 1;
      for (let i = 0; i < 400 && calls.length; i++) {
        const call = calls.shift()!;
        const ids = call.args.storyIds as string[];
        if (ids.some(out)) call.reject("arg_not_allowed");
        else call.resolve(answerFor(ids));
        await tick(0);
      }
      const ids = all.map((r) => r.getAttribute("data-id")!);
      expect(ids.filter((id) => !out(id)).every((id) => totals.get(id)?.kind === "ready")).toBe(
        true,
      );
      expect(
        ids
          .filter(out)
          .every((id) => (totals.get(id) as { error?: string } | undefined)?.error === "refused"),
      ).toBe(true);
      expect(warn).not.toHaveBeenCalled();
      totals.stop();
    });

    it("after an id was accepted, a batch whose probes are all refused marks only that batch: no verdict, later batches are asked and ready", async () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const { core, calls } = load(host);
      const { list, all } = rows(30);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      const idsOf = (from: number, to: number) =>
        all.slice(from, to).map((r) => r.getAttribute("data-id")!);
      see(all.slice(0, 10), true);
      await tick(200);
      calls.shift()!.resolve(answerFor(idsOf(0, 10)));
      await tick(0);
      see(all.slice(10, 20), true);
      await tick(200);
      for (let i = 0; i < 20 && calls.length; i++) {
        calls.shift()!.reject("arg_not_allowed");
        await tick(0);
      }
      expect(
        idsOf(10, 20).every(
          (id) => (totals.get(id) as { error?: string } | undefined)?.error === "refused",
        ),
      ).toBe(true);
      expect(warn).not.toHaveBeenCalled();
      see(all.slice(20), true);
      await tick(200);
      expect(calls.length).toBeGreaterThan(0);
      calls.shift()!.resolve(answerFor(idsOf(20, 30)));
      await tick(0);
      expect(totals.get("S-25")).toMatchObject({ kind: "ready" });
      expect(totals.get("S-3")).toMatchObject({ kind: "ready" });
      totals.stop();
    });

    it("after an answer, a batch whose first, middle and last ids are out of pin refuses only the ids that were refused: the rest is sifted and ready", async () => {
      // Fails before the class fix: the served reader marked the whole batch refused from its three probes.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const { core, calls } = load(host);
      const { list, all } = rows(20);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      const idsOf = (from: number, to: number) =>
        all.slice(from, to).map((r) => r.getAttribute("data-id")!);
      see(all.slice(0, 10), true);
      await tick(200);
      calls.shift()!.resolve(answerFor(idsOf(0, 10)));
      await tick(0);
      // Batch S-10..S-19: the probes S-10, S-15 and S-19 are out of pin; every other id is in pin.
      const out = new Set(["S-10", "S-15", "S-19"]);
      see(all.slice(10), true);
      await tick(200);
      for (let i = 0; i < 100 && calls.length; i++) {
        const call = calls.shift()!;
        const ids = call.args.storyIds as string[];
        if (ids.some((id) => out.has(id))) call.reject("arg_not_allowed");
        else call.resolve(answerFor(ids));
        await tick(0);
      }
      for (const id of idsOf(10, 20)) {
        if (out.has(id)) expect(totals.get(id)).toMatchObject({ error: "refused" });
        else expect(totals.get(id)).toMatchObject({ kind: "ready" });
      }
      expect(warn).not.toHaveBeenCalled();
      totals.stop();
    });

    it("an all-pending first answer counts as served: a later all-refused probe round gives no page verdict and no warning", async () => {
      // Fails before the class fix: everReady latched only on a ready entry, so the cold-index (all pending) page could be called unpinned.
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const { core, calls } = load(host);
      const { list, all } = rows(20);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      const idsOf = (from: number, to: number) =>
        all.slice(from, to).map((r) => r.getAttribute("data-id")!);
      see(all.slice(0, 10), true);
      await tick(200);
      calls.shift()!.resolve({
        stories: Object.fromEntries(idsOf(0, 10).map((id) => [id, { pending: true }])),
        reasons: [],
      });
      await tick(0);
      see(all.slice(10), true);
      await tick(200);
      for (let i = 0; i < 100 && calls.length; i++) {
        const call = calls.shift()!;
        const ids = call.args.storyIds as string[];
        if (ids.some((id) => Number(id.slice(2)) >= 10)) call.reject("arg_not_allowed");
        else
          call.resolve({
            stories: Object.fromEntries(ids.map((id) => [id, { pending: true }])),
            reasons: [],
          });
        await tick(0);
      }
      expect(warn).not.toHaveBeenCalled();
      expect(
        idsOf(10, 20).every(
          (id) => (totals.get(id) as { error?: string } | undefined)?.error === "refused",
        ),
      ).toBe(true);
      totals.stop();
    });

    it("the first-ever read of 2 rows, both refused: the verdict stands (documented residual), one warning, nothing asked after", async () => {
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      const { core, calls } = load(host);
      const { list, all } = rows(2);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      for (let i = 0; i < 10 && calls.length; i++) {
        calls.shift()!.reject("arg_not_allowed");
        await tick(0);
      }
      expect(warn).toHaveBeenCalledTimes(1);
      expect(totals.get("S-0")).toMatchObject({ error: "refused" });
      expect(totals.get("S-1")).toMatchObject({ error: "refused" });
      await tick(60_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    it("an id the list settled while its chase waited is not asked: no ask, no try counted", async () => {
      const { core, calls, serve } = load(host);
      core.budget({ minGapMs: 0 });
      const { list, all } = rows(10);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", answerFor(all.map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      core.dataChanged({ ids: ["S-3"] });
      await tick(200);
      serve("storyTotals", {
        stories: { "S-3": { totals: { costUsd: 1 }, computedAtMs: 500, refreshing: true } },
        reasons: [],
      });
      await tick(0);
      await tick(500);
      void totals.refresh();
      await tick(0);
      calls.shift()!.resolve(answerFor(all.map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      await tick(10_000);
      expect(calls.length).toBe(0);
      totals.stop();
    });

    describe("SV-PBF-300 reader rules", () => {
      it("the activity facets pass through get(id) like mergedAt: finite numbers or null, the reason only as a string", async () => {
        const { core, calls } = load(host);
        const reader = core.batchTotals(["A", "B", "C"]);
        const pulled = reader.pull();
        await tick(0);
        calls.shift()!.resolve({
          stories: {
            A: { ...story("A"), firstActivityAt: 50, lastActivityAt: 60, openSessions: 0 },
            B: {
              ...story("B"),
              firstActivityAt: null,
              lastActivityAt: "x",
              openSessions: null,
              activityReason: "no sessions",
            },
            C: story("C"),
          },
          reasons: [],
        });
        await pulled;
        expect(reader.get("A")).toMatchObject({
          firstActivityAt: 50,
          lastActivityAt: 60,
          openSessions: 0,
        });
        expect(reader.get("B")).toMatchObject({
          firstActivityAt: null,
          lastActivityAt: null,
          openSessions: null,
          activityReason: "no sessions",
        });
        expect(reader.get("C")!.firstActivityAt).toBeUndefined();
        expect(reader.get("C")!.openSessions).toBeUndefined();
        expect(reader.get("C")!.activityReason).toBeUndefined();
      });

      it("an answer settles only the event its own ask was made for", async () => {
        const { core, calls } = load(host);
        const reader = core.batchTotals(["A"]);
        const first = reader.pull();
        await tick(0);
        calls.shift()!.resolve(answerFor(["A"]));
        await first;
        reader.markDirty(["A"]);
        const one = reader.pull({ ids: ["A"] });
        await tick(0);
        reader.markDirty(["A"]);
        const two = reader.pull({ ids: ["A"] });
        await tick(0);
        calls.shift()!.resolve(answerFor(["A"]));
        await one;
        expect(reader.needs("A")).toBe(true);
        calls.shift()!.resolve(answerFor(["A"]));
        await two;
        expect(reader.needs("A")).toBe(false);
        reader.markDirty(["A"]);
        const three = reader.pull({ ids: ["A"] });
        await tick(0);
        reader.markDirty(["A"]);
        calls.shift()!.resolve(answerFor(["A"]));
        await three;
        expect(reader.needs("A")).toBe(true);
      });

      it("a kept row that answers pending drops the stale error", async () => {
        const { core, calls } = load(host);
        const reader = core.batchTotals(["A"], { retryable: () => true });
        const first = reader.pull();
        await tick(0);
        calls.shift()!.resolve(answerFor(["A"]));
        await first;
        const failed = reader.pull();
        await tick(0);
        calls.shift()!.resolve({ stories: { A: { error: "mac_timeout" } }, reasons: [] });
        await failed;
        expect(reader.get("A")).toMatchObject({ kind: "ready", kept: true, error: "mac_timeout" });
        const again = reader.pull();
        await tick(0);
        calls.shift()!.resolve({ stories: { A: { pending: true } }, reasons: [] });
        await again;
        expect(reader.get("A")).toMatchObject({ kind: "ready", refreshing: true });
        expect(reader.get("A")).not.toHaveProperty("error");
        expect(reader.get("A")).not.toHaveProperty("kept");
      });
    });

    describe("SV-PBF-300 refreshing rows and showing the page", () => {
      const refreshingAnswer = (ids: string[]) => ({
        stories: Object.fromEntries(
          ids.map((id) => [id, { totals: { costUsd: 1 }, computedAtMs: 500, refreshing: true }]),
        ),
        reasons: [],
      });
      async function open(n: number, answer: (ids: string[]) => unknown) {
        const loaded = load(host);
        loaded.core.budget({ minGapMs: 0 });
        const { list, all } = rows(n);
        const ids = all.map((r) => r.getAttribute("data-id")!);
        const totals = loaded.core.visibleTotals(list, { rowSelector: ".row" });
        see(all, true);
        await tick(200);
        expect(loaded.calls.length).toBe(1);
        loaded.serve("storyTotals", answer(ids));
        await tick(0);
        return { ...loaded, totals, all, ids };
      }

      it("a hide/show cycle (visibility, focus, an observer burst) with every row refreshing makes no ask", async () => {
        const { calls, totals, all } = await open(8, refreshingAnswer);
        setHidden(true);
        await tick(300_000);
        setHidden(false);
        see(all, false);
        see(all, true);
        await tick(0);
        await tick(5_000);
        expect(calls.length).toBe(0);
        await tick(54_000);
        expect(calls.length).toBe(0);
        // Not a show but the long interval: once, a full minute after the page came back.
        await tick(2_000);
        expect(calls.length).toBe(1);
        totals.stop();
      });

      it("follow-ups do not re-ask refreshing rows: once at 60 s, then 120 s later", async () => {
        const { calls, serve, ids, totals } = await open(8, refreshingAnswer);
        await tick(59_000);
        expect(calls.length).toBe(0);
        await tick(1_100);
        expect(calls.map((c) => c.args.storyIds)).toEqual([ids]);
        serve("storyTotals", refreshingAnswer(ids));
        await tick(0);
        await tick(119_000);
        expect(calls.length).toBe(0);
        await tick(1_100);
        expect(calls.length).toBe(1);
        totals.stop();
      });

      it("pending rows are still asked at 2, 4, 8, then 16 s, and only they", async () => {
        const { calls, serve, ids, totals } = await open(6, (all) => ({
          stories: Object.fromEntries(
            all.map((id, i) => [
              id,
              i < 2
                ? { pending: true }
                : { totals: { costUsd: 1 }, computedAtMs: 500, refreshing: true },
            ]),
          ),
          reasons: [],
        }));
        for (const wait of [2000, 4000, 8000, 16000]) {
          await tick(wait - 1);
          expect(calls.length).toBe(0);
          await tick(1);
          expect(calls.map((c) => c.args.storyIds)).toEqual([ids.slice(0, 2)]);
          serve("storyTotals", {
            stories: { [ids[0]]: { pending: true }, [ids[1]]: { pending: true } },
            reasons: [],
          });
          await tick(0);
        }
        totals.stop();
      });

      it("a named changed(ids) asks at once, whatever the long interval", async () => {
        const { calls, totals, core } = await open(8, refreshingAnswer);
        core.dataChanged({ ids: ["S-3"] });
        await tick(200);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-3"]]);
        totals.stop();
      });

      it("a pending row that scrolled away and back is asked by the follow-up timer 2 s after it returns, with no ask on the way in", async () => {
        const { calls, serve, all, ids, totals } = await open(3, (list) => ({
          stories: Object.fromEntries(
            list.map((id) => [
              id,
              id === "S-0" ? { pending: true } : { totals: { costUsd: 1 }, computedAtMs: 500 },
            ]),
          ),
          reasons: [],
        }));
        see([all[0]], false);
        await tick(60_000);
        expect(calls.length).toBe(0);
        see([all[0]], true);
        await tick(200);
        expect(calls.length).toBe(0);
        await tick(1_799);
        expect(calls.length).toBe(0);
        await tick(1);
        expect(calls.map((c) => c.args.storyIds)).toEqual([[ids[0]]]);
        serve("storyTotals", {
          stories: { "S-0": { totals: { costUsd: 1 }, computedAtMs: 600 } },
          reasons: [],
        });
        totals.stop();
      });

      it("a first ask that throws leaves its ids attempted: a hide/show and an observer burst ask nothing, the 2 s timer retries", async () => {
        const { core, calls, fail } = load(host);
        const { list, all } = rows(3);
        const totals = core.visibleTotals(list, { rowSelector: ".row" });
        see(all, true);
        await tick(200);
        fail("storyTotals", "mac_offline");
        await tick(0);
        setHidden(true);
        setHidden(false);
        see(all, false);
        see(all, true);
        await tick(1_999);
        expect(calls.length).toBe(0);
        await tick(1);
        expect(calls.map((c) => c.args.storyIds)).toEqual([
          all.map((r) => r.getAttribute("data-id")),
        ]);
        totals.stop();
      });

      it("a kept row whose refresh failed in transit is retried by the fast timer, not left to the long interval", async () => {
        const { calls, serve, totals, core } = await open(4, (list) => answerFor(list));
        core.dataChanged({ ids: ["S-1"] });
        await tick(200);
        serve("storyTotals", { stories: { "S-1": { error: "mac_timeout" } }, reasons: [] });
        await tick(0);
        expect(totals.get("S-1")).toMatchObject({ kind: "ready", kept: true, refreshing: true });
        await tick(1_999);
        expect(calls.length).toBe(0);
        await tick(1);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-1"]]);
        totals.stop();
      });

      it("a row never asked is asked when it scrolls in", async () => {
        const { core, calls, serve } = load(host);
        const { list, all } = rows(4);
        const totals = core.visibleTotals(list, { rowSelector: ".row" });
        see(all.slice(0, 2), true);
        await tick(200);
        serve("storyTotals", answerFor(["S-0", "S-1"]));
        await tick(0);
        see(all.slice(2, 3), true);
        await tick(200);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-2"]]);
        totals.stop();
      });

      it("changed(ids) while hidden, then a show with an observer burst, makes exactly one ask", async () => {
        const { calls, totals, core, all } = await open(4, (list) => answerFor(list));
        setHidden(true);
        core.dataChanged({ ids: ["S-1"] });
        await tick(5_000);
        expect(calls.length).toBe(0);
        setHidden(false);
        see(all, false);
        see(all, true);
        await tick(500);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-1"]]);
        totals.stop();
      });

      it("stop() clears the slow timer", async () => {
        const { calls, totals } = await open(4, refreshingAnswer);
        const pull = vi.spyOn(totals.reader, "pull");
        // Armed: the reader has rows to ask on the long interval.
        expect(totals.reader.slowIds()).toHaveLength(4);
        const armed = vi.getTimerCount();
        totals.stop();
        expect(vi.getTimerCount()).toBeLessThan(armed);
        // The slow timer would have fired at 60 s.
        await tick(300_000);
        expect(pull).not.toHaveBeenCalled();
        expect(calls.length).toBe(0);
      });

      it("the long interval backs off 60, 120, 240, 240 s and starts over at 60 s once nothing is refreshing", async () => {
        const { calls, totals, core, ids } = await open(4, refreshingAnswer);
        for (const wait of [60_000, 120_000, 240_000, 240_000]) {
          await tick(wait - 1_000);
          expect(calls.length).toBe(0);
          await tick(1_100);
          expect(calls).toHaveLength(1);
          calls.shift()!.resolve(refreshingAnswer(ids));
          await tick(0);
        }
        await tick(240_100);
        calls.shift()!.resolve(answerFor(ids));
        await tick(0);
        expect(totals.reader.slowIds()).toHaveLength(0);
        core.dataChanged({ ids: ["S-1"] });
        await tick(200);
        calls.shift()!.resolve({
          stories: { "S-1": { totals: { costUsd: 1 }, computedAtMs: 700, refreshing: true } },
          reasons: [],
        });
        await tick(0);
        await tick(59_000);
        expect(calls.length).toBe(0);
        await tick(1_100);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-1"]]);
        totals.stop();
      });

      it("a named ask that fails mac_offline, then a hide/show and an observer burst: no ask but the 2 s chase", async () => {
        const { calls, fail, totals, core, all } = await open(4, (list) => answerFor(list));
        core.dataChanged({ ids: ["S-1"] });
        await tick(200);
        fail("storyTotals", "mac_offline");
        await tick(0);
        setHidden(true);
        setHidden(false);
        see(all, false);
        see(all, true);
        await tick(1_000);
        expect(calls.length).toBe(0);
        await tick(1_000);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-1"]]);
        totals.stop();
      });

      it("a named ask that fails in transit stays dirty: retried at 2 s, then applied", async () => {
        const { calls, serve, fail, totals, core } = await open(4, (list) => answerFor(list));
        core.dataChanged({ ids: ["S-2"] });
        await tick(200);
        fail("storyTotals", "mac_offline");
        await tick(0);
        await tick(1_999);
        expect(calls.length).toBe(0);
        await tick(1);
        expect(calls.map((c) => c.args.storyIds)).toEqual([["S-2"]]);
        serve("storyTotals", {
          stories: { "S-2": { totals: { costUsd: 9 }, computedAtMs: 900 } },
          reasons: [],
        });
        await tick(0);
        expect(totals.get("S-2")).toMatchObject({ kind: "ready", computedAtMs: 900 });
        await tick(60_000);
        expect(calls.length).toBe(0);
      });
    });

    it("arg_not_allowed for one bad id among many: the probes pass, only that id is refused", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(20);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      for (let i = 0; i < 40 && calls.length; i++) {
        const call = calls.shift()!;
        const ids = call.args.storyIds as string[];
        if (ids.includes("S-5")) call.reject("arg_not_allowed");
        else call.resolve(answerFor(ids));
        await tick(0);
      }
      const ids = all.map((r) => r.getAttribute("data-id")!);
      expect(totals.get("S-5")).toMatchObject({ kind: "error", error: "refused" });
      expect(ids.filter((id) => totals.get(id)?.kind === "ready")).toHaveLength(19);
      totals.stop();
    });

    it("a thrown failed ask is transient: the whole chunk is asked again with backoff, never halved", async () => {
      const { core, calls, fail } = load(host);
      const { list, all } = rows(3);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      fail("storyTotals", "answer_failed");
      await tick(0);
      await tick(2000);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-0", "S-1", "S-2"]]);
      totals.stop();
    });

    it("a refused id leaves every automatic pull: live-event refreshes ask nothing, refresh({refused: true}) asks it", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(1);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      calls.shift()!.reject("query_invalid");
      await tick(0);
      expect(totals.get("S-0")).toMatchObject({ kind: "error", error: "refused" });
      for (let i = 0; i < 3; i++) {
        void totals.refresh();
        await tick(30_000);
      }
      expect(calls.length).toBe(0);
      void totals.refresh({ refused: true });
      await tick(0);
      expect(calls.length).toBe(1);
      totals.stop();
    });

    it("one live event for 1 of 50 visible rows: one storyTotals ask with that id, only that row's node changes, no list read", async () => {
      const { core, calls, serve } = load(host);
      const items = Array.from({ length: 50 }, (_, i) => `S-${i}`);
      const list = document.createElement("div");
      document.body.append(list);
      // Assigned after the first paint, which reads it as undefined.
      let totals: ReturnType<Core["visibleTotals"]> | undefined = undefined;
      const region = core.keyedRegion(list, "s");
      const paint = () =>
        region.paint(
          items,
          {
            key: (id: string) => id,
            version: (id: string) => String(totals?.get(id)?.computedAtMs ?? "-"),
            render: (id: string) => {
              const row = document.createElement("div");
              row.className = "row";
              row.setAttribute("data-id", id);
              row.textContent = `${id}:${totals?.get(id)?.computedAtMs ?? "-"}`;
              return row;
            },
          },
          "s",
        );
      paint();
      const listQuery = core.cachedQuery(
        "queryContext",
        { limit: 50 },
        { maxAgeMs: 60_000, key: "list" },
      );
      await tick(0);
      serve("queryContext", { rows: items });
      await listQuery.ready;
      totals = core.visibleTotals(list, { rowSelector: ".row", onChange: paint });
      see(Array.from(list.children), true);
      await tick(200);
      serve("storyTotals", answerFor(items));
      await tick(0);
      const before = Array.from(list.children);
      expect(before[7].textContent).toBe("S-7:111");
      core.dataChanged({ ids: ["S-7"] });
      await tick(200);
      // The list query is not read again: the only ask is storyTotals, for the one named id.
      expect(calls.map((c) => [c.name, c.args.storyIds])).toEqual([["storyTotals", ["S-7"]]]);
      serve("storyTotals", {
        stories: { "S-7": { totals: { costUsd: 2 }, computedAtMs: 999 } },
        reasons: [],
      });
      await tick(0);
      const after = Array.from(list.children);
      expect(after[7]).not.toBe(before[7]);
      expect(after[7].textContent).toBe("S-7:999");
      after.forEach((node, i) => {
        if (i !== 7) expect(node).toBe(before[i]);
      });
      expect(calls.length).toBe(0);
      totals.stop();
      region.stop();
    });

    it("a changed id that is not on screen is marked, and asked when it scrolls in; an event naming no ids does nothing", async () => {
      const { core, calls, serve } = load(host);
      const { list, all } = rows(50);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      serve("storyTotals", answerFor(all.map((r) => r.getAttribute("data-id")!)));
      await tick(0);
      see(all.slice(10), false);
      core.dataChanged({ ids: ["S-40"] });
      core.dataChanged({});
      core.dataChanged({ ids: [] });
      await tick(500);
      expect(calls.length).toBe(0);
      see([all[40]], true);
      await tick(200);
      expect(calls.map((c) => c.args.storyIds)).toEqual([["S-40"]]);
      totals.stop();
    });

    it("a verb refused for this page is known from one ask: 50 rows, 1 ask, none after until refresh({refused: true})", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(50);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      let asked = 0;
      for (let i = 0; i < 20 && calls.length; i++) {
        calls.shift()!.reject("query_not_declared");
        asked++;
        await tick(0);
      }
      expect(asked).toBe(1);
      expect(totals.get("S-3")).toMatchObject({ error: "refused" });
      await tick(60_000);
      expect(calls.length).toBe(0);
      void totals.refresh({ refused: true });
      await tick(0);
      expect(calls.length).toBeGreaterThan(0);
      totals.stop();
    });

    it.each([
      "arg_not_allowed",
      "query_invalid: args over 2 KB",
      "invalid_args: storyIds is not a list",
      'codesense bridge: command "storyTotals" is not allowlisted',
      "storyTotals: unknown field",
    ])(
      "a refusal in the wire's or the bridge's own words (%s) marks the id refused",
      async (message) => {
        const { core, calls } = load(host);
        const { list, all } = rows(1);
        const totals = core.visibleTotals(list, { rowSelector: ".row" });
        see(all, true);
        await tick(200);
        calls.shift()!.reject(message);
        await tick(0);
        expect(totals.get("S-0")).toMatchObject({ kind: "error", error: "refused" });
        await tick(30_000);
        expect(calls.length).toBe(0);
        totals.stop();
      },
    );

    it("a transient code with detail (no_answer: relay gap) is asked again with backoff", async () => {
      const { core, calls } = load(host);
      const { list, all } = rows(1);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(all, true);
      await tick(200);
      calls.shift()!.reject("no_answer: relay gap");
      await tick(0);
      await tick(2000);
      expect(calls.length).toBe(1);
      totals.stop();
    });

    it("rows that arrive later are watched, and the page can refresh every visible id in one call", async () => {
      const { core, calls, serve } = load(host);
      core.budget({ minGapMs: 0 });
      const { list } = rows(0);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      const row = document.createElement("div");
      row.className = "row";
      row.setAttribute("data-id", "LATE");
      list.append(row);
      await tick(0);
      see([row], true);
      await tick(200);
      expect(calls[0].args.storyIds).toEqual(["LATE"]);
      serve("storyTotals", answerFor(["LATE"]));
      await tick(0);
      const refreshed = totals.refresh();
      await tick(0);
      expect(calls[0].args.storyIds).toEqual(["LATE"]);
      serve("storyTotals", answerFor(["LATE"]));
      await refreshed;
      totals.stop();
    });
  });

  describe("K2 paced", () => {
    it("a busy Mac is asked again with backoff, never failed; a burst is one trailing run", async () => {
      const { core } = load(host);
      const run = vi.fn().mockRejectedValueOnce(new Error("mac_busy")).mockResolvedValue("ok");
      const paced = core.paced(run, { key: "p" });
      const answers = Promise.all([paced.request({}), paced.request({}), paced.request({})]);
      await tick(0);
      expect(run).toHaveBeenCalledTimes(1);
      await tick(1000);
      expect(run).toHaveBeenCalledTimes(2);
      // The two requests made after the first run began share the one trailing run, minGapMs (2 s) after it started.
      await tick(2000);
      expect(run).toHaveBeenCalledTimes(3);
      await expect(answers).resolves.toEqual(["ok", "ok", "ok"]);
      paced.stop();
    });
    it("a dead cursor restarts the read silently", async () => {
      const { core } = load(host);
      const run = vi
        .fn()
        .mockRejectedValueOnce(new Error("stale_cursor"))
        .mockResolvedValue("page-1");
      const paced = core.paced(run);
      const answer = paced.request({});
      await tick(0);
      await expect(answer).resolves.toBe("page-1");
      expect(run).toHaveBeenCalledTimes(2);
      paced.stop();
    });
    it("runs reads no closer than minGapMs", async () => {
      const { core } = load(host);
      const run = vi.fn().mockResolvedValue(1);
      const paced = core.paced(run, { minGapMs: 2000 });
      await paced.request({}, { force: true });
      const second = paced.request({}, { force: true });
      await tick(1999);
      expect(run).toHaveBeenCalledTimes(1);
      await tick(1);
      await second;
      expect(run).toHaveBeenCalledTimes(2);
      paced.stop();
    });
  });

  describe("K8 section", () => {
    function details() {
      const el = document.createElement("details");
      el.innerHTML = "<summary>Turns</summary>";
      document.body.append(el);
      const open = () => {
        el.open = true;
        el.dispatchEvent(new Event("toggle"));
      };
      return { el, open, text: () => el.querySelector(".cs-section-body")!.textContent ?? "" };
    }
    const options = (extra: Record<string, unknown> = {}) => ({
      key: "turns",
      scope: "story-A",
      load: ({ read }: { read: (n: string, a?: Record<string, unknown>) => Promise<unknown> }) =>
        read("sessionRead", { limit: 20 }),
      render: (value: unknown[], into: HTMLElement) => {
        into.textContent = `${value.length} turns`;
      },
      ...extra,
    });

    it("reads once when opened, shows its own skeleton, then the content with its age", async () => {
      const { core, calls, serve } = load(host);
      const d = details();
      core.section(d.el, options());
      await tick(0);
      expect(calls.length).toBe(0);
      d.open();
      await tick(0);
      expect(d.el.querySelector(".cs-section-skeleton")).not.toBeNull();
      serve("sessionRead", [1, 2, 3]);
      await tick(0);
      expect(d.text()).toContain("3 turns");
      expect(d.text()).toContain("Read just now");
      await tick(30_000 * 4);
      expect(d.text()).toContain("Read 2 min ago");
      expect(calls.length).toBe(0);
    });

    it("says empty, unknown with its reason, and error with Retry that reads again", async () => {
      const { core, serve, fail, calls } = load(host);
      const d = details();
      core.section(d.el, options());
      d.open();
      await tick(0);
      serve("sessionRead", []);
      await tick(0);
      expect(d.text()).toContain("Nothing here yet.");

      const u = details();
      core.section(
        u.el,
        options({ key: "u", load: async () => ({ unknown: "turns are not recorded" }) }),
      );
      u.open();
      await tick(0);
      expect(u.text()).toContain("Unknown · Turns are not recorded");

      const e = details();
      core.section(e.el, options({ key: "e" }));
      e.open();
      await tick(0);
      fail("sessionRead", "boom");
      await tick(0);
      expect(e.text()).toContain("Could not read this");
      e.el.querySelector<HTMLButtonElement>(".cs-section-meta button")!.click();
      await tick(0);
      expect(calls.some((c) => c.name === "sessionRead")).toBe(true);
    });

    it("an answer for a scope that was left is dropped (old after new)", async () => {
      const { core, calls } = load(host);
      const d = details();
      let scope = "A";
      const section = core.section(d.el, options({ scope: () => scope }));
      d.open();
      await tick(0);
      const late = calls[0];
      scope = "B";
      void section.refresh();
      await tick(0);
      calls[1].resolve([1, 2]);
      await tick(0);
      late.resolve([9, 9, 9, 9, 9]);
      await tick(0);
      expect(d.text()).toContain("2 turns");
      expect(d.text()).not.toContain("5 turns");
    });

    it("a closed scope drops its late answer", async () => {
      const { core, calls } = load(host);
      const d = details();
      core.section(d.el, options());
      d.open();
      await tick(0);
      core.closeScope("story-A");
      calls[0].resolve([1]);
      await tick(0);
      expect(d.text()).not.toContain("1 turns");
    });

    it("still reading shows after stillMs with Cancel, and a cancelled read's answer is dropped", async () => {
      const { core, calls } = load(host);
      const d = details();
      core.section(d.el, options({ stillMs: 15_000 }));
      d.open();
      await tick(0);
      await tick(14_999);
      expect(d.text()).not.toContain("Still reading");
      await tick(1);
      expect(d.text()).toContain("Still reading (15 s)");
      d.el.querySelector<HTMLButtonElement>(".cs-section-meta button")!.click();
      await tick(0);
      expect(d.text()).toContain("Cancelled.");
      calls[0].resolve([1, 2]);
      await tick(0);
      expect(d.text()).toContain("Cancelled.");
    });

    it("a visible section reads when it scrolls into view, not before", async () => {
      const { core, calls } = load(host);
      const el = document.createElement("section");
      document.body.append(el);
      core.section(el, options({ when: "visible" }));
      await tick(0);
      expect(calls.length).toBe(0);
      see([el], true);
      await tick(0);
      expect(calls.length).toBe(1);
    });
  });

  describe("budget, waiting and sections under pressure", () => {
    function details() {
      const el = document.createElement("details");
      el.innerHTML = "<summary>More</summary>";
      document.body.append(el);
      const open = () => {
        el.open = true;
        el.dispatchEvent(new Event("toggle"));
      };
      return { el, open, text: () => el.querySelector(".cs-section-body")!.textContent ?? "" };
    }
    const sectionOptions = (key: string, extra: Record<string, unknown> = {}) => ({
      key,
      scope: "s",
      load: ({ read }: { read: (n: string, a?: Record<string, unknown>) => Promise<unknown> }) =>
        read("sessionRead", { sessionId: key }),
      render: (value: unknown[], into: HTMLElement) => {
        into.textContent = `${value.length} items`;
      },
      ...extra,
    });

    it("Cancel frees the section's budget slot at once, so the next queued read starts", async () => {
      const { core, calls } = load(host);
      core.budget({ inFlight: 2 });
      const a = details();
      const b = details();
      const first = core.section(a.el, sectionOptions("a"));
      core.section(b.el, sectionOptions("b"));
      a.open();
      b.open();
      await tick(0);
      core.cachedQuery("third", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["sessionRead", "sessionRead"]);
      first.cancel();
      await tick(0);
      expect(calls.map((c) => c.name)).toEqual(["sessionRead", "sessionRead", "third"]);
    });

    it("a read that never answers frees its slot after timeoutMs", async () => {
      const { core, calls } = load(host);
      core.budget({ inFlight: 1, timeoutMs: 5000 });
      const stuck = core.cachedQuery("stuck", {}, { maxAgeMs: 1000, key: "t" });
      core.cachedQuery("next", {}, { maxAgeMs: 1000, key: "t" });
      await tick(4999);
      expect(calls.map((c) => c.name)).toEqual(["stuck"]);
      await tick(1);
      expect(calls.map((c) => c.name)).toEqual(["stuck", "next"]);
      expect(stuck.peek().error?.message).toBe("timeout");
    });

    it("reads in one slot start at least minGapMs apart, and minGapMs 0 turns the gap off", async () => {
      const { core, calls } = load(host);
      const handle = core.cachedQuery("g", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      void handle.refresh();
      await tick(1999);
      expect(calls.length).toBe(1);
      await tick(1);
      expect(calls.length).toBe(2);

      resetPage();
      const open = load(host);
      open.core.budget({ minGapMs: 0 });
      const quick = open.core.cachedQuery("g", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      void quick.refresh();
      await tick(0);
      expect(open.calls.length).toBe(2);
    });

    it("query, queryAll and totals share the one budget: at most 2 in flight", async () => {
      const { core, calls } = load(host);
      core.budget({ inFlight: 2 });
      void core.queryAll("queryContext", {}, { limit: 2 });
      void core.queryAll("sessionUsage", {}, { limit: 2 });
      void core.query("x", {});
      void core.batchTotals(["A"]).pull();
      await tick(0);
      expect(calls.length).toBe(2);
      expect(core.budget().stats()).toMatchObject({ inFlight: 2, queued: 2 });
    });

    it("a busy Mac is Waiting for the Mac, never an error, and it clears when the read answers", async () => {
      const { core, calls, serve, fail } = load(host);
      const handle = core.cachedQuery("q", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      expect(core.budget().state()).toBe("idle");
      fail("q", "busy");
      await tick(0);
      expect(core.budget().state()).toBe("waiting");
      expect(core.valueState(null, { waiting: core.budget().stats().busy }).text).toBe(
        "Waiting for the Mac",
      );
      expect(core.valueState(null, { error: new Error("mac_busy") }).text).toBe(
        "Waiting for the Mac",
      );
      expect(core.valueState(4, { waiting: true }).text).toBe("4");
      await tick(1300);
      expect(calls.filter((c) => c.name === "q")).toHaveLength(1);
      serve("q", { ok: 1 });
      await handle.ready;
      expect(core.budget().state()).toBe("idle");
      expect(handle.peek()).toMatchObject({ state: "fresh", error: null });
    });

    it("a section says Waiting for the Mac while the Mac is busy, and asks again after 2, 4, 8 s", async () => {
      const { core } = load(host);
      const read = vi.fn().mockRejectedValue(new Error("mac_busy"));
      const d = details();
      core.section(d.el, sectionOptions("w", { load: read }));
      d.open();
      await tick(0);
      expect(d.text()).toContain("Waiting for the Mac");
      expect(d.text()).not.toContain("Could not read");
      expect(read).toHaveBeenCalledTimes(1);
      await tick(2000);
      expect(read).toHaveBeenCalledTimes(2);
      await tick(4000);
      expect(read).toHaveBeenCalledTimes(3);
      await tick(8000);
      expect(read).toHaveBeenCalledTimes(4);
    });

    it("a clock tick rewrites only the status text: content and focus survive it", async () => {
      const { core, serve } = load(host);
      const d = details();
      core.section(
        d.el,
        sectionOptions("tick", {
          render: (_v: unknown[], into: HTMLElement) => {
            into.innerHTML = "<button id=keep>go</button><p>hello</p>";
          },
        }),
      );
      d.open();
      await tick(0);
      serve("sessionRead", [1]);
      await tick(0);
      const content = d.el.querySelector(".cs-section-value")!;
      const button = d.el.querySelector<HTMLButtonElement>("#keep")!;
      button.focus();
      await tick(60_000);
      expect(d.el.querySelector(".cs-section-value")).toBe(content);
      expect(d.el.querySelector("#keep")).toBe(button);
      expect(document.activeElement).toBe(button);
      expect(d.text()).toContain("Read 1 min ago");
    });

    it("a slow but legitimate read is not cut off: a 120 s storyChanges answers", async () => {
      const { core, serve } = load(host);
      // A shared page's own wire gives up after 20 s; the budget adds no timeout of its own below the engine's 900 s.
      const wait = host === "app" ? 120_000 : 15_000;
      const handle = core.cachedQuery(
        "storyChanges",
        { storyId: "S-1", limit: 20 },
        { maxAgeMs: 1000, key: "t" },
      );
      await tick(wait);
      serve("storyChanges", { commits: [], total: 0 });
      await handle.ready;
      expect(handle.peek()).toMatchObject({ state: "fresh", error: null });
    });

    // The slot owns the read. Whoever acts (the same instance, a second instance of the same key and scope, another scope), the
    // pending engine calls, the budget's in-flight reads, the elapsed text and the painted answer follow the slot.
    type Actor = "same" | "second" | "other";
    const SLOT_CASES: [Actor, string, number, number, string[], string[]][] = [
      ["same", "run", 1, 1, ["Still reading (16 s)"], []],
      ["second", "run", 1, 1, ["Still reading (16 s)"], []],
      ["other", "run", 2, 1, [], ["Still reading"]],
      ["same", "force", 2, 1, [], ["Still reading"]],
      ["second", "force", 2, 1, [], ["Still reading"]],
      ["other", "force", 2, 1, [], ["Still reading"]],
      ["same", "cancel", 1, 0, ["Cancelled."], []],
      ["second", "cancel", 1, 0, ["Cancelled."], []],
      ["other", "cancel", 1, 1, [], ["Cancelled."]],
      ["same", "lands", 0, 0, ["1 items"], []],
      ["second", "lands", 0, 0, ["1 items"], []],
      ["other", "lands", 0, 0, [], ["1 items"]],
      ["same", "fails", 0, 0, ["Could not read this."], []],
      ["second", "fails", 0, 0, ["Could not read this."], []],
      ["other", "fails", 0, 0, [], ["Could not read this"]],
    ];
    it.each(SLOT_CASES)(
      "slot table: %s instance, %s",
      async (actor, action, pending, inFlight, has, hasNot) => {
        const { core, calls, serve, fail } = load(host);
        const a = details();
        const b = details();
        const c = details();
        const sa = core.section(a.el, sectionOptions("table", { scope: "s" }));
        const sb = core.section(b.el, sectionOptions("table", { scope: "s" }));
        const sc = core.section(c.el, sectionOptions("table", { scope: "t" }));
        a.open();
        await tick(16_000);
        expect(calls.length).toBe(1);
        const target = { same: { d: a, s: sa }, second: { d: b, s: sb }, other: { d: c, s: sc } }[
          actor
        ];
        if (actor === "second") b.open();
        if (action === "run" && actor === "same") {
          a.el.open = false;
          a.el.dispatchEvent(new Event("toggle"));
          a.open();
        }
        if (action === "run" && actor === "other") c.open();
        if (action === "force") void target.s.refresh();
        if (action === "cancel") target.s.cancel();
        if (action === "lands") serve("sessionRead", [1]);
        if (action === "fails") fail("sessionRead", "boom");
        await tick(0);
        expect(calls.length).toBe(pending);
        expect(core.budget().stats().inFlight).toBe(inFlight);
        for (const text of has) expect(target.d.text()).toContain(text);
        for (const text of hasNot) expect(target.d.text()).not.toContain(text);
      },
    );

    // A slot has one way to end. Whatever ends it (another scope, its scope closed, its last view leaving), the busy retry that was
    // armed never fires and no stray read follows.
    it.each(["switch", "closeScope", "stop"])(
      "a busy error then %s: the armed retry is gone, no stray read",
      async (how) => {
        const { core, calls } = load(host);
        let scope = "s";
        const busy = vi.fn().mockRejectedValue(new Error("mac_busy"));
        const a = details();
        const sa = core.section(
          a.el,
          sectionOptions("dispose", { scope: () => scope, load: busy }),
        );
        a.open();
        await tick(0);
        expect(busy).toHaveBeenCalledTimes(1);
        await tick(1000);
        if (how === "switch") {
          scope = "t";
          const b = details();
          core.section(b.el, sectionOptions("dispose", { scope: () => scope }));
          b.open();
          await tick(0);
          expect(calls.map((c) => c.name)).toEqual(["sessionRead"]);
          expect(core.budget().stats().inFlight).toBe(1);
        }
        if (how === "closeScope") core.closeScope("s");
        if (how === "stop") sa.stop();
        await tick(30_000);
        expect(busy).toHaveBeenCalledTimes(1);
        expect(calls.length).toBe(how === "switch" ? 1 : 0);
      },
    );

    it("stop() then a new section of the same key and scope in the same tick: a fresh read, no cancelled error", async () => {
      const { core, calls } = load(host);
      const a = details();
      const first = core.section(a.el, sectionOptions("same-tick"));
      a.open();
      await tick(0);
      first.stop();
      const b = details();
      core.section(b.el, sectionOptions("same-tick"));
      b.open();
      await tick(0);
      expect(calls.length).toBe(2);
      calls[1].resolve([1]);
      await tick(0);
      expect(b.text()).toContain("1 items");
      expect(b.text()).not.toContain("Could not read");
    });

    it("stop() disposes the slot the view last painted, even after the page moved its scope on", async () => {
      const { core } = load(host);
      let selected = "A";
      const a = details();
      const view = core.section(a.el, sectionOptions("moved", { scope: () => selected }));
      a.open();
      await tick(0);
      expect(core.budget().stats().inFlight).toBe(1);
      selected = "B";
      view.stop();
      await tick(0);
      expect(core.budget().stats().inFlight).toBe(0);
    });

    it("a read a minute old offers Refresh, and Refresh reads again", async () => {
      const { core, calls, serve } = load(host);
      const d = details();
      core.section(d.el, sectionOptions("old"));
      d.open();
      await tick(0);
      serve("sessionRead", [1]);
      await tick(0);
      expect(d.el.querySelector(".cs-section-meta button")).toBeNull();
      await tick(60_000);
      const button = d.el.querySelector<HTMLButtonElement>(".cs-section-meta button")!;
      expect(button.textContent).toBe("Refresh");
      button.click();
      await tick(0);
      expect(calls.filter((c) => c.name === "sessionRead")).toHaveLength(1);
    });

    it("budget backpressure shows the normal loading; only a busy Mac shows Waiting; after stillMs it is Still reading", async () => {
      const { core, fail } = load(host);
      core.budget({ inFlight: 1 });
      core.cachedQuery("hold", {}, { maxAgeMs: 1000, key: "t" });
      const d = details();
      core.section(d.el, sectionOptions("queued"));
      d.open();
      await tick(0);
      expect(d.el.querySelector(".cs-section-skeleton")).not.toBeNull();
      expect(d.text()).not.toContain("Waiting for the Mac");
      fail("hold", "busy");
      await tick(0);
      expect(d.text()).toContain("Waiting for the Mac");
      await tick(15_000);
      expect(d.text()).toContain("Still reading (15 s)");
      expect(d.text()).not.toContain("Waiting for the Mac");
    });

    it("Cancel during a busy wait stops the retries and clears Waiting at once", async () => {
      const { core, calls, fail } = load(host);
      const d = details();
      const section = core.section(d.el, sectionOptions("busy-cancel"));
      d.open();
      await tick(0);
      fail("sessionRead", "busy");
      await tick(0);
      expect(core.budget().stats().busy).toBe(true);
      section.cancel();
      await tick(0);
      expect(core.budget().stats().busy).toBe(false);
      const before = calls.length;
      await tick(30_000);
      expect(calls.length).toBe(before);
      expect(d.text()).toContain("Cancelled.");
    });

    it("a hosted read waits for the viewer's hello", async () => {
      if (host !== "hosted") return;
      withHello = false;
      const { core, calls } = load("hosted");
      withHello = true;
      core.cachedQuery("q", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      expect(calls.length).toBe(0);
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { cs: "live", v: 1, op: "hello", role: "viewer", control: false },
          source: shellParent as Window,
        }),
      );
      await tick(0);
      expect(calls.length).toBe(1);
    });

    it("the wire's silence reads as a sentence in CS.value too", () => {
      const { core } = load(host);
      expect(core.errorText(new Error("no_answer"))).toBe("The Mac didn't answer in time");
      expect(core.errorText("timeout")).toBe("The Mac didn't answer in time");
      expect(core.errorText(new Error("boom"))).toBe("Could not read this");
      expect(core.valueState(null, { error: new Error("no_answer") })).toEqual({
        state: "error",
        text: "Not loaded · The Mac didn't answer in time",
      });
      expect(core.valueState(null, { error: true }).text).toBe("Not loaded");
    });

    it("errorText: a sentence for each known code, never a raw code or [object Object]", () => {
      const { core } = load(host);
      const silent = "The Mac didn't answer in time";
      const refused = "This page can't show this";
      const generic = "Could not read this";
      const table: [unknown, string][] = [
        [new Error("no_answer"), silent],
        ["timeout", silent],
        ["mac_timeout", silent],
        ["mac_offline", "The Mac is offline"],
        [new Error("mac_busy"), "The Mac is busy"],
        ["busy", "The Mac is busy"],
        ["rate_limited", "The Mac is busy"],
        ["arg_not_allowed", refused],
        ["query_not_declared", refused],
        ["query_invalid", refused],
        ["invalid_args", refused],
        [new Error("not_approved"), refused],
        ["not_declared", refused],
        ["undeclared", refused],
        ["refused", refused],
        ['codesense bridge: command "storyTotals" is not allowlisted', refused],
        ["storyTotals: unknown field", refused],
        ["access_ended", "Access to this dashboard has ended"],
        ["answer_failed", "The Mac couldn't answer"],
        ["answer_too_large", "Too much to show at once"],
        ["failed", "The Mac couldn't read this"],
        [new Error("boom"), generic],
        ["mystery_code", generic],
        [{ message: "x" }, generic],
        [null, generic],
        [undefined, generic],
        [42, generic],
      ];
      for (const [input, text] of table) expect(core.errorText(input)).toBe(text);
    });

    it("the wire's silence reads as a sentence, never as a code", async () => {
      const { core } = load(host);
      const d = details();
      core.section(
        d.el,
        sectionOptions("quiet", { load: () => Promise.reject(new Error("no_answer")) }),
      );
      d.open();
      await tick(0);
      expect(d.text()).toContain("The Mac didn't answer in time");
      expect(d.text()).not.toContain("no_answer");
    });
  });

  describe("tabbing away and back never reloads", () => {
    const detailsEl = () => {
      const el = document.createElement("details");
      el.innerHTML = "<summary>More</summary>";
      document.body.append(el);
      const open = () => {
        el.open = true;
        el.dispatchEvent(new Event("toggle"));
      };
      return { el, open };
    };
    const sectionOpts = (key: string, extra: Record<string, unknown> = {}) => ({
      key,
      scope: "s",
      load: ({ read }: { read: (n: string, a?: Record<string, unknown>) => Promise<unknown> }) =>
        read("sessionRead", { sessionId: key }),
      render: (value: unknown[], into: HTMLElement) => {
        into.textContent = `${value.length} items`;
      },
      ...extra,
    });

    it("a hide/show cycle (visibility and window focus) makes zero reads and keeps scroll, focus and the list's DOM", async () => {
      const { core, calls, serve } = load(host);
      const list = document.createElement("div");
      for (let i = 0; i < 6; i++) {
        const row = document.createElement("div");
        row.className = "row";
        row.setAttribute("data-id", `S-${i}`);
        list.append(row);
      }
      const button = document.createElement("button");
      document.body.append(list, button);
      const rowEls = Array.from(list.children);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(rowEls, true);
      await tick(200);
      serve("storyTotals", answerFor(rowEls.map((r) => r.getAttribute("data-id")!)));
      const query = core.cachedQuery("list", {}, { maxAgeMs: 1000, key: "t" });
      await tick(0);
      serve("list", { rows: [] });
      await query.ready;
      const opened = detailsEl();
      core.section(opened.el, sectionOpts("nr"));
      opened.open();
      await tick(0);
      serve("sessionRead", [1]);
      const seen = document.createElement("section");
      document.body.append(seen);
      core.section(seen, sectionOpts("nr-visible", { when: "visible" }));
      see([seen], true);
      await tick(0);
      serve("sessionRead", [2]);
      await tick(0);
      const renders = vi.fn();
      if (host === "app") core.live!(renders, { pollMs: 30_000, events: [] });
      await tick(0);
      list.scrollTop = 120;
      button.focus();
      const snapshot = list.innerHTML + button.outerHTML;
      const reads = core.budget().stats().calls;
      const rendered = renders.mock.calls.length;
      expect(calls.length).toBe(0);

      setHidden(true);
      await tick(600_000);
      setHidden(false);
      // Back in view: the observers fire again, a closed details opens again, the network is back.
      see([seen], true);
      opened.el.open = false;
      opened.el.dispatchEvent(new Event("toggle"));
      opened.open();
      window.dispatchEvent(new Event("online"));
      await tick(0);

      expect(core.budget().stats().calls).toBe(reads);
      expect(calls.length).toBe(0);
      expect(renders.mock.calls.length).toBe(rendered);
      expect(list.scrollTop).toBe(120);
      expect(document.activeElement).toBe(button);
      expect(list.innerHTML + button.outerHTML).toBe(snapshot);
      // A minute after show the timers have started again, and still nothing was read (a poll, if the page has one, only renders).
      await tick(60_000);
      expect(core.budget().stats().calls).toBe(reads);
      expect(calls.length).toBe(0);
      expect(list.scrollTop).toBe(120);
      totals.stop();
    });

    it("a stream watch keeps running while hidden: no unwatch, no new watch on return", async () => {
      if (host !== "hosted") return;
      const { core } = load("hosted");
      const watch = core.watch!("terminals", {}, {});
      await tick(0);
      const streamOps = () => postedOps.filter((op) => op === "watch" || op === "unwatch");
      expect(streamOps()).toHaveLength(1);
      setHidden(true);
      await tick(60_000);
      setHidden(false);
      await tick(0);
      expect(streamOps()).toHaveLength(1);
      watch.stop();
    });

    it("an observed element is read once however often it re-enters view; refresh reads it again", async () => {
      const { core } = load(host);
      const run = vi.fn().mockResolvedValue(1);
      const paced = core.paced(run, { key: "obs", minGapMs: 0 });
      const el = document.createElement("div");
      document.body.append(el);
      paced.observe(el, { id: 1 });
      see([el], true);
      await tick(0);
      see([el], false);
      see([el], true);
      await tick(0);
      expect(run).toHaveBeenCalledTimes(1);
      paced.refresh();
      await tick(0);
      expect(run).toHaveBeenCalledTimes(2);
      paced.stop();
    });
  });

  describe("round 14: sections, pauses and cleanups", () => {
    const detailsEl = () => {
      const el = document.createElement("details");
      el.innerHTML = "<summary>More</summary>";
      document.body.append(el);
      const open = () => {
        el.open = true;
        el.dispatchEvent(new Event("toggle"));
      };
      return { el, open, text: () => el.querySelector(".cs-section-body")!.textContent ?? "" };
    };
    const secOpts = (key: string, extra: Record<string, unknown> = {}) => ({
      key,
      scope: "s",
      load: ({ read }: { read: (n: string, a?: Record<string, unknown>) => Promise<unknown> }) =>
        read("sessionRead", { sessionId: key }),
      render: (value: unknown[], into: HTMLElement) => {
        into.textContent = `${value.length} items`;
      },
      ...extra,
    });

    it("dataChanged({keys}) re-reads only the sections it names, in place", async () => {
      const { core, calls, serve } = load(host);
      const a = detailsEl();
      const b = detailsEl();
      core.section(a.el, secOpts("named"));
      core.section(b.el, secOpts("other"));
      a.open();
      b.open();
      await tick(0);
      serve("sessionRead", [1]);
      serve("sessionRead", [1, 2]);
      await tick(0);
      const content = a.el.querySelector(".cs-section-value");
      core.dataChanged({ keys: ["named"] });
      await tick(0);
      expect(calls.map((c) => c.args.sessionId)).toEqual(["named"]);
      serve("sessionRead", [1, 2, 3]);
      await tick(0);
      expect(a.text()).toContain("3 items");
      expect(b.text()).toContain("2 items");
      expect(a.el.querySelector(".cs-section-value")).not.toBeNull();
      expect(content).not.toBeNull();
      core.dataChanged({ keys: ["nobody"] });
      core.dataChanged({ ids: ["S-1"] });
      await tick(0);
      expect(calls.length).toBe(0);
    });

    it("scrolling back in or reopening after Cancel or an error reads nothing: only Retry does", async () => {
      const { core, calls, fail } = load(host);
      const failing = detailsEl();
      core.section(failing.el, secOpts("failing"));
      failing.open();
      await tick(0);
      fail("sessionRead", "answer_failed");
      await tick(0);
      expect(failing.text()).toContain("Could not read this");
      failing.el.open = false;
      failing.el.dispatchEvent(new Event("toggle"));
      failing.open();
      await tick(10_000);
      expect(calls.length).toBe(0);
      failing.el.querySelector<HTMLButtonElement>(".cs-section-meta button")!.click();
      await tick(0);
      expect(calls.length).toBe(1);

      const cancelled = detailsEl();
      const view = core.section(cancelled.el, secOpts("cancelled"));
      cancelled.open();
      await tick(0);
      view.cancel();
      await tick(0);
      const before = calls.length;
      cancelled.el.open = false;
      cancelled.el.dispatchEvent(new Event("toggle"));
      cancelled.open();
      await tick(10_000);
      expect(calls.length).toBe(before);
      expect(cancelled.text()).toContain("Cancelled.");
    });

    it("a render that throws shows the error state, not a blank section", async () => {
      vi.spyOn(console, "error").mockImplementation(() => {});
      const { core, serve } = load(host);
      const d = detailsEl();
      core.section(
        d.el,
        secOpts("throws", {
          render: () => {
            throw new Error("render broke");
          },
        }),
      );
      d.open();
      await tick(0);
      serve("sessionRead", [1]);
      await tick(0);
      expect(d.text()).toContain("Could not show this.");
    });

    it("a busy retry waits while hidden and starts its timer again on show: nothing is read on return", async () => {
      const { core } = load(host);
      const read = vi.fn().mockRejectedValue(new Error("mac_busy"));
      const d = detailsEl();
      core.section(d.el, secOpts("busy-hidden", { load: read }));
      d.open();
      await tick(0);
      expect(read).toHaveBeenCalledTimes(1);
      await tick(1000);
      setHidden(true);
      await tick(30_000);
      expect(read).toHaveBeenCalledTimes(1);
      setHidden(false);
      await tick(1999);
      expect(read).toHaveBeenCalledTimes(1);
      await tick(1);
      expect(read).toHaveBeenCalledTimes(2);
    });

    it("an event that names ids or keys while hidden asks nothing until shown, then one batch and one key at a time", async () => {
      const { core, calls, serve } = load(host);
      const list = document.createElement("div");
      for (let i = 0; i < 4; i++) {
        const row = document.createElement("div");
        row.className = "row";
        row.setAttribute("data-id", `S-${i}`);
        list.append(row);
      }
      document.body.append(list);
      const rowEls = Array.from(list.children);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see(rowEls, true);
      await tick(200);
      serve("storyTotals", answerFor(rowEls.map((r) => r.getAttribute("data-id")!)));
      const d = detailsEl();
      core.section(d.el, secOpts("event-hidden"));
      d.open();
      await tick(0);
      serve("sessionRead", [1]);
      await tick(0);
      setHidden(true);
      core.dataChanged({ ids: ["S-1", "S-2"], keys: ["event-hidden"] });
      await tick(30_000);
      expect(calls.length).toBe(0);
      setHidden(false);
      await tick(200);
      expect(calls.map((c) => c.args.storyIds ?? c.args.sessionId)).toEqual([["S-1", "S-2"]]);
      serve("storyTotals", answerFor(["S-1", "S-2"]));
      await tick(1000);
      expect(calls.map((c) => c.args.sessionId)).toEqual(["event-hidden"]);
      totals.stop();
    });

    it("the budget validates what it is given: inFlight is at least 1, a bad value keeps the setting", () => {
      const { core } = load(host);
      expect(core.budget().config().inFlight).toBe(2);
      core.budget({ inFlight: 0 });
      core.budget({ inFlight: Number.NaN });
      core.budget({ inFlight: -3, timeoutMs: 0, minGapMs: -1 });
      expect(core.budget().config()).toMatchObject({
        inFlight: 2,
        timeoutMs: 900_000,
        minGapMs: 2000,
      });
      core.budget({ inFlight: 3.9 });
      expect(core.budget().config().inFlight).toBe(3);
    });

    it("stopping a list or a paced reader frees its budgeted read", async () => {
      const { core, calls } = load(host);
      const list = document.createElement("div");
      const row = document.createElement("div");
      row.className = "row";
      row.setAttribute("data-id", "S-0");
      list.append(row);
      document.body.append(list);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      see([row], true);
      await tick(200);
      expect(core.budget().stats().inFlight).toBe(1);
      totals.stop();
      await tick(0);
      expect(core.budget().stats().inFlight).toBe(0);

      const paced = core.paced("slow", { key: "stop", minGapMs: 0 });
      void paced.request({ n: 1 }).catch(() => {});
      await tick(0);
      expect(core.budget().stats().inFlight).toBe(1);
      paced.stop();
      await tick(0);
      expect(core.budget().stats().inFlight).toBe(0);
      expect(calls.length).toBe(2);
    });

    it("rows that left the container are no longer watched", async () => {
      const { core } = load(host);
      const list = document.createElement("div");
      const rowEls = [0, 1, 2].map((i) => {
        const row = document.createElement("div");
        row.className = "row";
        row.setAttribute("data-id", `S-${i}`);
        list.append(row);
        return row;
      });
      document.body.append(list);
      const totals = core.visibleTotals(list, { rowSelector: ".row" });
      expect(watchers.some((w) => w.seen.has(rowEls[1]))).toBe(true);
      rowEls[1].remove();
      await tick(0);
      expect(watchers.some((w) => w.seen.has(rowEls[1]))).toBe(false);
      expect(watchers.some((w) => w.seen.has(rowEls[0]))).toBe(true);
      // Re-attached (a filter that removes and restores nodes): it is watched again. Fails before r15: the row kept its watched flag.
      list.append(rowEls[1]);
      await tick(0);
      expect(watchers.some((w) => w.seen.has(rowEls[1]))).toBe(true);
      totals.stop();
    });

    it("a cachedQuery asked again with another maxAgeMs gets its own handle; an entry without a numeric time is nothing", async () => {
      const { core } = load(host);
      const a = core.cachedQuery("q", {}, { maxAgeMs: 1000, key: "t" });
      const same = core.cachedQuery("q", {}, { maxAgeMs: 1000, key: "t" });
      const other = core.cachedQuery("q", {}, { maxAgeMs: 5000, key: "t" });
      expect(same).toBe(a);
      expect(other).not.toBe(a);
      window.localStorage.setItem(
        `cs-kit:1:${host}:t:bad:{}`,
        JSON.stringify({ at: "yesterday", value: 1 }),
      );
      expect(core.cachedQuery("bad", {}, { maxAgeMs: 1000, key: "t" }).peek().state).toBe("none");
    });

    it("keyedRegion without a version re-renders a row whose content changed", () => {
      const { core } = load(host);
      const root = document.createElement("div");
      document.body.append(root);
      const region = core.keyedRegion(root, "A");
      const opts = {
        key: (i: { id: string }) => i.id,
        render: (i: { id: string; t: string }) => {
          const row = document.createElement("div");
          row.textContent = i.t;
          return row;
        },
      };
      region.paint(
        [
          { id: "1", t: "a" },
          { id: "2", t: "x" },
        ],
        opts,
        "A",
      );
      const before = Array.from(root.children);
      region.paint(
        [
          { id: "1", t: "b" },
          { id: "2", t: "x" },
        ],
        opts,
        "A",
      );
      const after = Array.from(root.children);
      expect(after[0]).not.toBe(before[0]);
      expect(after[0].textContent).toBe("b");
      expect(after[1]).toBe(before[1]);
      region.stop();
    });

    it("a busy retry waits while its <details> is closed, and opening starts the timer again", async () => {
      const { core } = load(host);
      const read = vi.fn().mockRejectedValue(new Error("mac_busy"));
      const d = detailsEl();
      core.section(d.el, secOpts("busy-closed", { load: read }));
      d.open();
      await tick(0);
      expect(read).toHaveBeenCalledTimes(1);
      d.el.open = false;
      d.el.dispatchEvent(new Event("toggle"));
      await tick(30_000);
      expect(read).toHaveBeenCalledTimes(1);
      d.open();
      await tick(1999);
      expect(read).toHaveBeenCalledTimes(1);
      await tick(1);
      expect(read).toHaveBeenCalledTimes(2);
    });

    it("a key an event named while hidden, before its section existed, is consumed when the section appears", async () => {
      const { core, calls, serve } = load(host);
      setHidden(true);
      core.dataChanged({ keys: ["late"] });
      setHidden(false);
      const d = detailsEl();
      core.section(d.el, secOpts("late"));
      d.open();
      await tick(0);
      serve("sessionRead", [1]);
      await tick(0);
      await tick(5000);
      setHidden(true);
      setHidden(false);
      await tick(5000);
      expect(calls.length).toBe(0);
    });

    it("a paced reader forgets elements that left the document", () => {
      const { core } = load(host);
      const paced = core.paced(vi.fn().mockResolvedValue(1), { key: "detached", minGapMs: 0 });
      const gone = document.createElement("div");
      const kept = document.createElement("div");
      document.body.append(gone, kept);
      paced.observe(gone, { id: 1 });
      gone.remove();
      paced.observe(kept, { id: 2 });
      expect(watchers.some((w) => w.seen.has(gone))).toBe(false);
      expect(watchers.some((w) => w.seen.has(kept))).toBe(true);
      paced.stop();
    });

    it("a paced reader keeps rows built before they are attached (r16: only a node once connected is forgotten)", () => {
      const { core } = load(host);
      const paced = core.paced(vi.fn().mockResolvedValue(1), { key: "prebuilt", minGapMs: 0 });
      const list = document.createElement("div");
      const row1 = document.createElement("div");
      const row2 = document.createElement("div");
      list.append(row1, row2);
      paced.observe(row1, { id: 1 });
      paced.observe(row2, { id: 2 });
      document.body.append(list);
      expect(watchers.some((w) => w.seen.has(row1))).toBe(true);
      expect(watchers.some((w) => w.seen.has(row2))).toBe(true);
      paced.stop();
    });

    it("content that cannot be stringified (circular) repaints and never throws", () => {
      const { core } = load(host);
      const root = document.createElement("div");
      document.body.append(root);
      const region = core.keyedRegion(root, "A");
      const item: Record<string, unknown> = { id: "1" };
      item.self = item;
      const opts = {
        key: (i: { id: string }) => i.id,
        render: () => document.createElement("div"),
      };
      expect(() => region.paint([item], opts, "A")).not.toThrow();
      const first = root.children[0];
      expect(() => region.paint([item], opts, "A")).not.toThrow();
      expect(root.children[0]).not.toBe(first);
      region.stop();
    });

    it("an unchanged hub resync delivers nothing: no render; a delta or a different snapshot does", async () => {
      if (host !== "app") return;
      const { core } = load("app");
      const renders = vi.fn();
      core.live!(renders, { events: ["sessions-changed"] });
      await tick(0);
      expect(renders).toHaveBeenCalledTimes(1);
      const snap = (rev: number, rows: unknown[]) => ({
        topic: "sessions",
        viewKey: "k",
        rev,
        snapshot: rows,
      });
      hubChange!(snap(1, [{ id: "a" }]));
      await tick(0);
      expect(renders).toHaveBeenCalledTimes(2);
      hubChange!(snap(2, [{ id: "a" }]));
      await tick(0);
      expect(renders).toHaveBeenCalledTimes(2);
      hubChange!({
        topic: "sessions",
        viewKey: "k",
        rev: 3,
        upserts: [{ id: "a", x: 1 }],
        removes: [],
      });
      await tick(0);
      expect(renders).toHaveBeenCalledTimes(3);
      hubChange!(snap(4, [{ id: "a" }]));
      await tick(0);
      expect(renders).toHaveBeenCalledTimes(4);
    });
  });

  describe("K10 keyedRegion", () => {
    it("repaints rows in place, keeping the node and focus of a row that moves", () => {
      const { core } = load(host);
      const root = document.createElement("div");
      document.body.append(root);
      const region = core.keyedRegion(root, "A");
      const render = (item: { id: string }) => {
        const row = document.createElement("div");
        row.setAttribute("data-row-focus", item.id);
        const b = document.createElement("button");
        b.textContent = item.id;
        row.append(b);
        return row;
      };
      const opts = { key: (i: { id: string }) => i.id, render, version: () => "v1" };
      region.paint([{ id: "1" }, { id: "2" }], opts, "A");
      const second = root.children[1] as HTMLElement;
      const button = second.querySelector("button")!;
      button.focus();
      const order = () => Array.from(root.children).map((n) => (n as HTMLElement).dataset.key);
      // The row holding focus is protected: a reorder waits, and focus stays where it was.
      region.paint([{ id: "2" }, { id: "1" }], opts, "A");
      expect(document.activeElement).toBe(button);
      expect(order()).toEqual(["1", "2"]);
      button.blur();
      region.flush();
      expect(order()).toEqual(["2", "1"]);
      expect(root.children[0]).toBe(second);
      region.stop();
    });
    it("drops a paint for a scope that is no longer current", () => {
      const { core } = load(host);
      const root = document.createElement("div");
      document.body.append(root);
      const region = core.keyedRegion(root, "A");
      region.setScope("B");
      const opts = { key: (i: { id: string }) => i.id, render: () => document.createElement("i") };
      expect(region.paint([{ id: "1" }], opts, "A")).toBe(false);
      expect(root.children.length).toBe(0);
      expect(region.paint([{ id: "1" }], opts, "B")).toBe(true);
      region.stop();
    });
  });

  describe("K1 boot", () => {
    it.runIf(host === "app")(
      "paints the cached answer before any read returns, and again when the read lands",
      async () => {
        const first = load(host);
        first.core.cachedQuery("list", {}, { maxAgeMs: 60_000, key: "page" });
        await tick(0);
        first.serve("list", { rows: ["old"] });
        await tick(10);
        resetKeepStorage();
        const { core, serve } = load(host);
        const seen: unknown[] = [];
        const app = core.boot({
          key: "page",
          queries: { list: { name: "list", args: {}, maxAgeMs: 1 } },
          render: (ctx: { state: { data: Record<string, { value: unknown; state: string }> } }) =>
            seen.push([ctx.state.data.list.state, ctx.state.data.list.value]),
        });
        expect(seen[0]).toEqual(["stale", { rows: ["old"] }]);
        await tick(0);
        serve("list", { rows: ["new"] });
        await tick(0);
        expect(seen.at(-1)).toEqual(["fresh", { rows: ["new"] }]);
        app.stop();
      },
    );
    it("is ready once the host answers, and a render that throws goes to onError", async () => {
      const { core } = load(host);
      const onError = vi.fn();
      const app = core.boot({
        render: () => {
          throw new Error("render broke");
        },
        onError,
      });
      expect(onError).toHaveBeenCalled();
      await tick(0);
      expect(app.state.ready).toBe(true);
    });
  });
});

// A new page load keeps localStorage (a reopen), unlike resetPage.
function resetKeepStorage() {
  const saved = storedKeys().map((k) => [k, window.localStorage.getItem(k) as string]);
  resetPage();
  saved.forEach(([k, v]) => window.localStorage.setItem(k, v));
}
