// SPDX-License-Identifier: MIT
import { testAnyListPaging } from "./paging-contract.test.fixtures";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// SV-ENG-63: real-contract tests for CS.queryAll / CS.subscribeAll / CS.partialList
// in the hosted toolkit. WIRE v1; no protocol migration in this batch.

const SOURCE = readFileSync(
  join(process.cwd(), "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
  "utf8",
);

type Posted = Record<string, unknown>;
type Answer = {
  ok: boolean;
  answer?: unknown;
  error?: string;
  id?: string;
  source?: string;
  answered_at?: number;
  server_time?: number;
};
type Result = {
  rows: unknown[];
  total: number | null;
  complete: boolean;
  truncated: boolean;
  returned: number;
  loading?: boolean;
  error?: Error | null;
  canLoadMore?: boolean;
  atCeiling?: boolean;
  waitingForHead?: boolean;
  source?: string;
  answered_at?: number;
  server_time?: number;
  totals?: unknown;
  days?: unknown;
};
type ViewerState = {
  role: "owner" | "viewer";
  mac: "online" | "offline" | "unknown" | null;
  mac_seen_at: number | null;
  updated_at: number | null;
  stale: boolean;
  display: string | null;
};
type CS = {
  viewer: (cb: (state: ViewerState) => void) => () => void;
  queryAll: (
    name: string,
    args: Record<string, unknown>,
    opts?: Record<string, unknown>,
  ) => Promise<Result>;
  subscribeAll: (
    name: string,
    args: Record<string, unknown>,
    everyS: number,
    cb: (r: Result) => void,
    opts?: Record<string, unknown>,
  ) => (() => void) & { loadMore: () => Promise<Result>; refresh: () => Promise<Result> };
  partialList: (opts: Record<string, unknown>) => {
    el: HTMLElement;
    stop: () => void;
    loadMore: () => Promise<Result>;
    refresh: () => Promise<Result>;
  };
};

function loadToolkit() {
  const posted: Posted[] = [];
  const parent = {};
  const listeners: EventListener[] = [];
  Object.defineProperty(window, "parent", { value: parent, configurable: true });
  vi.spyOn(window, "postMessage").mockImplementation(((msg: Posted) => {
    posted.push(msg);
  }) as typeof window.postMessage);
  const originalAdd = window.addEventListener.bind(window);
  const addSpy = vi.spyOn(window, "addEventListener").mockImplementation(((
    type: string,
    handler: EventListenerOrEventListenerObject,
  ) => {
    const fn = typeof handler === "function" ? handler : handler.handleEvent.bind(handler);
    if (type === "message") listeners.push(fn);
    originalAdd(type, fn);
  }) as typeof window.addEventListener);
  // One core per page: a test that loads the toolkit again starts from a fresh budget.
  delete (window as unknown as Record<string, unknown>).__csKitCore;
  const CS = new Function(`${SOURCE}\nreturn CS;`)() as CS;
  addSpy.mockRestore();
  window.dispatchEvent(
    new MessageEvent("message", {
      data: { cs: "live", v: 1, op: "hello", role: "viewer", control: false },
      source: parent as Window,
    }),
  );

  let fallbackId = 0;
  function answer(id: string, a: Answer) {
    window.dispatchEvent(
      new MessageEvent("message", {
        data: Object.assign({ cs: "live", v: 1, op: "answer", id }, a),
        source: parent as Window,
      }),
    );
  }
  function answerNext(a: Answer) {
    const msg = posted[posted.length - 1];
    const id = typeof msg?.id === "string" ? msg.id : `q${++fallbackId}`;
    answer(id, a);
  }
  function cleanup() {
    listeners.forEach((fn) => window.removeEventListener("message", fn));
    listeners.length = 0;
  }

  return { CS, posted, answer, answerNext, cleanup };
}

function queryIds(posted: Posted[]) {
  return posted.filter((m) => m.op === "query").map((m) => m.id as string);
}
function subId(posted: Posted[]) {
  const m = posted.find((m) => m.op === "subscribe");
  return m?.id as string | undefined;
}
function lastQueryId(posted: Posted[]) {
  const ids = queryIds(posted);
  return ids[ids.length - 1];
}

function session(id: string, lastAtMs: number) {
  return { sessionId: id, lastAtMs, tokens: { input: 1, output: 0 } };
}
function sessionPage(
  rows: unknown[],
  total: number,
  nextCursor?: string,
  totals?: unknown,
  days?: unknown,
) {
  const a: any = { sessions: rows, total };
  if (nextCursor) a.nextCursor = nextCursor;
  if (totals !== undefined) a.totals = totals;
  if (days !== undefined) a.days = days;
  return a;
}
function contextPage(rows: unknown[], total: number, nextCursor?: string) {
  const a: any = { rows, total };
  if (nextCursor) a.nextCursor = nextCursor;
  return a;
}
function idsOf(rows: unknown[]) {
  return rows.map((r: any) => r.sessionId || r.id || r.path).sort();
}

async function drain() {
  await vi.advanceTimersByTimeAsync(0);
}

describe("CS.viewer", () => {
  afterEach(() => vi.restoreAllMocks());

  it("delivers viewer changes, replays current state, and stops only the unsubscribed callback", () => {
    const h = loadToolkit();
    const first = vi.fn<(state: ViewerState) => void>();
    const second = vi.fn<(state: ViewerState) => void>();
    const stopFirst = h.CS.viewer(first);
    let stopSecond: (() => void) | undefined;
    const online: ViewerState = {
      role: "viewer",
      mac: "online",
      mac_seen_at: null,
      updated_at: null,
      stale: false,
      display: "viewer@example.com",
    };
    const offline: ViewerState = { ...online, mac: "offline" };
    const stale: ViewerState = { ...offline, stale: true };
    try {
      expect(first).not.toHaveBeenCalled();
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { cs: "live", v: 1, op: "viewer", ...online },
          source: window.parent,
        }),
      );
      expect(first.mock.calls).toEqual([[online]]);
      stopSecond = h.CS.viewer(second);
      expect(second.mock.calls).toEqual([[online]]);
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { cs: "live", v: 1, op: "viewer", ...offline },
          source: window.parent,
        }),
      );
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { cs: "live", v: 1, op: "viewer", ...stale },
          source: window.parent,
        }),
      );
      expect(first.mock.calls).toEqual([[online], [offline], [stale]]);
      expect(second.mock.calls).toEqual([[online], [offline], [stale]]);
      stopFirst();
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { cs: "live", v: 1, op: "viewer", ...online },
          source: window.parent,
        }),
      );
      expect(first.mock.calls).toEqual([[online], [offline], [stale]]);
      expect(second.mock.calls).toEqual([[online], [offline], [stale], [online]]);
      expect(h.posted.filter((m) => m.op === "viewer_sub")).toEqual([
        { op: "viewer_sub", cs: "live", v: 1 },
      ]);
    } finally {
      stopFirst();
      stopSecond?.();
      h.cleanup();
    }
  });
});

// --- queryAll ---

describe("CS.queryAll", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("walks sessionUsage pages and preserves totals/days from page 1", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const totals = { costUsd: 1.23, tokens: { input: 100, output: 50 } };
      const days = [{ day: "2026-10-25" }];
      const p = CS.queryAll("sessionUsage", { sinceMs: 0 }, { limit: 150 });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: sessionPage(
          Array.from({ length: 150 }, (_, i) => session(`s${i}`, 1000 - i)),
          201,
          "c1",
          totals,
          days,
        ),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: sessionPage(
          Array.from({ length: 51 }, (_, i) => session(`s${150 + i}`, 500 - i)),
          201,
        ),
      });
      const r = await p;
      expect(r.rows).toHaveLength(201);
      expect(r.total).toBe(201);
      expect(r.complete).toBe(true);
      expect(r.truncated).toBe(false);
      expect(r.totals).toEqual(totals);
      expect(r.days).toEqual(days);
      expect(queryIds(posted)).toHaveLength(2);
      // Wire v1 envelope used for every query.
      expect(
        posted.filter((m) => m.op === "query").every((m) => m.v === 1 && m.cs === "live"),
      ).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("walks queryContext rows and nextCursor", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("queryContext", { text: "x" }, { limit: 2 });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: contextPage([{ id: "a" }, { id: "b" }], 4, "c1"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: contextPage([{ id: "c" }, { id: "d" }], 4),
      });
      const r = await p;
      expect(idsOf(r.rows)).toEqual(["a", "b", "c", "d"]);
      expect(r.complete).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("forwards original args and effective limit on every page", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("queryContext", { text: "x", where: "y" }, { limit: 3 });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "a" }], 2, "c1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "b" }], 2) });
      await p;
      const queries = posted.filter((m) => m.op === "query");
      expect(queries[0].args).toEqual(expect.objectContaining({ text: "x", where: "y", limit: 3 }));
      expect(queries[1].args).toEqual(
        expect.objectContaining({ text: "x", where: "y", limit: 3, cursor: "c1" }),
      );
    } finally {
      cleanup();
    }
  });

  it("rejects on ordinary first-page errors", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {});
      await drain();
      answer(lastQueryId(posted)!, { ok: false, error: "not_allowed" });
      await expect(p).rejects.toThrow("not_allowed");
    } finally {
      cleanup();
    }
  });

  it("keeps one query in flight", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      CS.queryAll("sessionUsage", {});
      await drain();
      expect(queryIds(posted)).toHaveLength(1);
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([], 0) });
    } finally {
      cleanup();
    }
  });

  it("backs off on rate_limited and retries", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {});
      await drain();
      answer(lastQueryId(posted)!, { ok: false, error: "rate_limited" });
      await vi.advanceTimersByTimeAsync(5000);
      expect(queryIds(posted)).toHaveLength(2);
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("x", 1)], 1) });
      const r = await p;
      expect(r.rows).toHaveLength(1);
    } finally {
      cleanup();
    }
  });

  it("gives up after six rate_limited retries", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {}, { limit: 1 });
      await drain();
      // Initial failure + 6 retries = 7 requests before rejection.
      for (let i = 0; i < 7; i++) {
        answer(lastQueryId(posted)!, { ok: false, error: "rate_limited" });
        if (i < 6) await vi.advanceTimersByTimeAsync(5000 * (i + 1));
      }
      await expect(p).rejects.toThrow("rate_limited");
      expect(queryIds(posted)).toHaveLength(7);
    } finally {
      cleanup();
    }
  });

  it("restarts once on query_invalid continuation then rejects on repeat", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {}, { limit: 1 });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("a", 10)], 2, "c1") });
      await drain();
      // Continuation fails with query_invalid.
      answer(lastQueryId(posted)!, { ok: false, error: "query_invalid" });
      await drain();
      // Restart from page 1 is posted.
      expect(queryIds(posted)).toHaveLength(3);
      const restartId = lastQueryId(posted)!;
      // Restart also fails => reject.
      answer(restartId, { ok: false, error: "query_invalid" });
      await expect(p).rejects.toThrow("query_invalid");
    } finally {
      cleanup();
    }
  });

  it("SV-PBF-287: walks again from the head on stale_cursor, silently, keeping only the new walk's rows", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("queryContext", {}, { limit: 2 });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: contextPage([{ id: "gone" }, { id: "a" }], 3, "c1"),
      });
      await drain();
      for (let i = 0; i < 3; i++) {
        answer(lastQueryId(posted)!, { ok: false, error: "stale_cursor" });
        await drain();
        expect(
          (posted.filter((m) => m.op === "query").at(-1)?.args as Record<string, unknown>).cursor,
        ).toBeUndefined();
        answer(lastQueryId(posted)!, {
          ok: true,
          answer: contextPage([{ id: "a" }, { id: "b" }], 3, `r${i}`),
        });
        await drain();
        expect(posted.filter((m) => m.op === "query").at(-1)?.args).toMatchObject({
          cursor: `r${i}`,
          limit: 2,
        });
      }
      answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "c" }], 3) });
      const r = await p;
      expect(idsOf(r.rows)).toEqual(["a", "b", "c"]);
      expect(r.complete).toBe(true);
      expect(r.error).toBeNull();
    } finally {
      cleanup();
    }
  });

  it("SV-PBF-287: busy and mac_busy are asked again with backoff; the sixth busy rejects", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    const query = (CS as unknown as { query: (name: string, args: unknown) => Promise<Answer> })
      .query;
    try {
      const p = query("storyTotals", { storyIds: ["SV-1"] });
      await drain();
      answer(lastQueryId(posted)!, { ok: false, error: "busy" });
      await drain();
      expect(queryIds(posted)).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(1250);
      expect(queryIds(posted)).toHaveLength(2);
      answer(lastQueryId(posted)!, { ok: false, error: "mac_busy" });
      await vi.advanceTimersByTimeAsync(2250);
      expect(queryIds(posted)).toHaveLength(3);
      answer(lastQueryId(posted)!, { ok: true, answer: { stories: {} } });
      await expect(p).resolves.toMatchObject({ ok: true, answer: { stories: {} } });

      const again = query("storyTotals", { storyIds: ["SV-1"] });
      const rejected = expect(again).rejects.toThrow("busy");
      for (const wait of [1250, 2250, 4250, 8250, 8250]) {
        await drain();
        answer(lastQueryId(posted)!, { ok: false, error: "busy" });
        await vi.advanceTimersByTimeAsync(wait);
      }
      await drain();
      answer(lastQueryId(posted)!, { ok: false, error: "busy" });
      await rejected;
      expect(queryIds(posted)).toHaveLength(3 + 6);
    } finally {
      cleanup();
    }
  });

  it("SV-PBF-287 M3: CS.query asks again after the relay's rate_limited", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    const query = (CS as unknown as { query: (name: string, args: unknown) => Promise<Answer> })
      .query;
    try {
      const p = query("storyTotals", { storyIds: ["SV-1"] });
      await drain();
      answer(lastQueryId(posted)!, { ok: false, error: "rate_limited" });
      await drain();
      expect(queryIds(posted)).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(1250);
      expect(queryIds(posted)).toHaveLength(2);
      answer(lastQueryId(posted)!, { ok: true, answer: { stories: {} } });
      await expect(p).resolves.toMatchObject({ ok: true, answer: { stories: {} } });
    } finally {
      cleanup();
    }
  });

  it("SV-PBF-287 H2: CS.storyTotals asks in parts under 2 KB, asks again for rest, and merges one answer", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    type Merged = {
      stories: Record<string, { unavailable?: Record<string, number> }>;
      reasons: string[];
      pending: string[];
      indexing: unknown;
      source: string;
      answered_at: number;
    };
    const storyTotals = (CS as unknown as { storyTotals: (ids: string[]) => Promise<Merged> })
      .storyTotals;
    try {
      const ids = Array.from({ length: 300 }, (_, i) => `SV-PBF-${1000 + i}`);
      const p = storyTotals(ids);
      await drain();
      // The page's read budget carries two asks at once; the third waits for a slot.
      expect(posted.filter((m) => m.op === "query")).toHaveLength(2);
      const last = ids[299];
      const reply = (m: Posted, i: number) => {
        const part = (m.args as { storyIds: string[] }).storyIds.filter((id) => id !== last);
        answer(m.id as string, {
          ok: true,
          source: i === 1 ? "cache" : "live",
          answered_at: 100 + i,
          answer: {
            stories: Object.fromEntries(
              part.map((id) => [id, { totals: { sessions: 1 }, unavailable: { costUsd: 0 } }]),
            ),
            reasons: [i === 1 ? "B" : "A"],
            pending: [],
            indexing: { done: 299, total: 300 },
            ...(i === 2 ? { truncated: true, rest: [last] } : {}),
          },
        });
      };
      posted.filter((m) => m.op === "query").forEach(reply);
      await drain();
      const asks = posted.filter((m) => m.op === "query");
      expect(asks).toHaveLength(3);
      reply(asks[2], 2);
      const parts = asks.map((m) => (m.args as { storyIds: string[] }).storyIds);
      expect(parts.flat()).toEqual(ids);
      for (const m of asks) {
        expect(m.query).toBe("storyTotals");
        expect(new TextEncoder().encode(JSON.stringify(m.args)).length).toBeLessThanOrEqual(1900);
      }
      await drain();
      const again = posted.filter((m) => m.op === "query");
      expect(again).toHaveLength(4);
      expect(again[3].args).toEqual({ storyIds: [last] });
      answer(again[3].id as string, {
        ok: true,
        source: "live",
        answered_at: 200,
        answer: {
          stories: { [last]: { pending: true } },
          reasons: [],
          pending: [last],
          indexing: { done: 299, total: 300 },
        },
      });
      const merged = await p;
      expect(Object.keys(merged.stories)).toHaveLength(300);
      expect(merged.reasons).toEqual(["A", "B"]);
      expect(merged.stories[parts[0][0]].unavailable).toEqual({ costUsd: 0 });
      expect(merged.stories[parts[1][0]].unavailable).toEqual({ costUsd: 1 });
      expect(merged.stories[parts[2][0]].unavailable).toEqual({ costUsd: 0 });
      expect(merged.pending).toEqual([last]);
      expect(merged.source).toBe("cache");
      expect(merged.answered_at).toBe(100);
    } finally {
      cleanup();
    }
  });

  it("SV-PBF-287: a fourth stale_cursor in one walk rejects", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("queryContext", {}, { limit: 1 });
      const rejected = expect(p).rejects.toThrow("stale_cursor");
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "a" }], 2, "c0") });
      await drain();
      for (let i = 0; i < 3; i++) {
        answer(lastQueryId(posted)!, { ok: false, error: "stale_cursor" });
        await drain();
        answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "a" }], 2, `r${i}`) });
        await drain();
      }
      answer(lastQueryId(posted)!, { ok: false, error: "stale_cursor" });
      await rejected;
      expect(queryIds(posted)).toHaveLength(8);
    } finally {
      cleanup();
    }
  });

  it("rejects a second invalid continuation after the cursorless restart succeeds", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const pending = CS.queryAll("sessionUsage", {}, { limit: 1 });
      const rejected = expect(pending).rejects.toThrow("query_invalid");
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("a", 10)], 3, "c1") });
      await drain();
      expect(posted.filter((m) => m.op === "query").at(-1)?.args).toMatchObject({
        cursor: "c1",
        limit: 1,
      });
      answer(lastQueryId(posted)!, { ok: false, error: "query_invalid" });
      await drain();
      expect(queryIds(posted)).toHaveLength(3);
      expect(
        (posted.filter((m) => m.op === "query").at(-1)?.args as Record<string, unknown>).cursor,
      ).toBeUndefined();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("a", 11)], 3, "c2") });
      await drain();
      expect(posted.filter((m) => m.op === "query").at(-1)?.args).toMatchObject({
        cursor: "c2",
        limit: 1,
      });
      answer(lastQueryId(posted)!, { ok: false, error: "query_invalid" });
      await rejected;
      await drain();
      expect(queryIds(posted)).toHaveLength(4);
    } finally {
      cleanup();
    }
  });

  it("recovers moved middle rows by union (limit 1, real IDs A,B,C)", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {}, { limit: 1 });
      await drain();
      // First walk sees A then B; C was not yet in the window.
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("A", 30)], 3, "c1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("B", 20)], 3, "c2") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([], 3) });
      await drain();
      // First full recovery rewalk: A, C, B.
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("A", 30)], 3, "c1b") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("C", 25)], 3, "c2b") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("B", 20)], 3) });
      const r = await p;
      expect(idsOf(r.rows)).toEqual(["A", "B", "C"]);
      expect(r.complete).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("recovers eight-session movers across multiple pages (limit 2)", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {}, { limit: 2 });
      await drain();
      // Initial walk (sorted by lastAtMs desc): A80,B70 | D50,E40 | C30,B20 -- wait, real pages
      // should be stable sorted windows. Use a plausible trace: first pass misses G/H,
      // second pass catches movers.
      const page = (rows: unknown[], cursor?: string) => sessionPage(rows, 8, cursor);
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("A", 80), session("B", 70)], "c1"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("C", 60), session("D", 50)], "c2"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("E", 40), session("F", 30)], "c3"),
      });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page([session("B", 20)]) });
      await drain();
      // End of first walk; B is duplicated but total 8 means G,H missing => recovery.
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("A", 80), session("B", 70)], "c1b"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("C", 60), session("D", 50)], "c2b"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("E", 40), session("F", 30)], "c3b"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: page([session("G", 25), session("H", 15)]),
      });
      const r = await p;
      expect(idsOf(r.rows)).toEqual(["A", "B", "C", "D", "E", "F", "G", "H"]);
      expect(r.complete).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("reports latest N of M after two full rewalks exhaust recovery budget", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {}, { limit: 1 });
      const page = (id: string, cursor?: string) => sessionPage([session(id, 1)], 5, cursor);
      // Initial walk: a,b,c,d (e is missing because it moved above the cursor).
      answer(lastQueryId(posted)!, { ok: true, answer: page("a", "1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("b", "2") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("c", "3") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("d") });
      await drain();
      // Rewalk 1 (same missing e).
      answer(lastQueryId(posted)!, { ok: true, answer: page("a", "1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("b", "2") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("c", "3") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("d") });
      await drain();
      // Rewalk 2.
      answer(lastQueryId(posted)!, { ok: true, answer: page("a", "1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("b", "2") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("c", "3") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: page("d") });
      const r = await p;
      expect(r.rows).toHaveLength(4);
      expect(r.total).toBe(5);
      expect(r.complete).toBe(false);
      expect(r.truncated).toBe(false);
      expect(r.returned).toBe(4);
      // One initial walk + two cursorless recovery rewalks = 12 requests.
      expect(queryIds(posted)).toHaveLength(12);
    } finally {
      cleanup();
    }
  });

  it("stops at maxRows and reports truthful truncated", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const p = CS.queryAll("sessionUsage", {}, { limit: 1, maxRows: 2 });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("a", 1)], 5, "1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("b", 1)], 5, "2") });
      const r = await p;
      expect(r.rows).toHaveLength(2);
      expect(r.complete).toBe(false);
      expect(r.truncated).toBe(true);
      expect(r.total).toBe(5);
    } finally {
      cleanup();
    }
  });

  it("emits loading and onPage updates", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const pages: Result[] = [];
      const p = CS.queryAll("queryContext", {}, { limit: 1, onPage: (r: Result) => pages.push(r) });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "a" }], 2, "1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: contextPage([{ id: "b" }], 2) });
      await p;
      expect(pages.some((r) => r.loading)).toBe(true);
      expect(pages[pages.length - 1].complete).toBe(true);
    } finally {
      cleanup();
    }
  });
});

// --- subscribeAll ---

describe("CS.subscribeAll", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("does an initial full walk on the first tick", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      CS.subscribeAll("sessionUsage", { sinceMs: 0 }, 15, (r) => results.push(r), { limit: 1 });
      await drain();
      const sid = subId(posted);
      expect(sid).toBeDefined();
      answer(sid!, { ok: true, answer: sessionPage([session("a", 1)], 2, "c1") });
      await drain();
      const qid = lastQueryId(posted);
      expect(qid).toBeDefined();
      answer(qid!, { ok: true, answer: sessionPage([session("b", 1)], 2) });
      await drain();
      const last = results[results.length - 1];
      expect(last.rows).toHaveLength(2);
      expect(last.complete).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("updates page 1 on ticks and preserves cached rows", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      CS.subscribeAll("sessionUsage", {}, 15, (r) => results.push(r), { limit: 2 });
      await drain();
      const sid = subId(posted)!;
      answer(sid, { ok: true, answer: sessionPage([session("a", 30), session("b", 20)], 3, "c1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("c", 10)], 3) });
      await drain();
      expect(results[results.length - 1].complete).toBe(true);
      expect(idsOf(results[results.length - 1].rows)).toEqual(["a", "b", "c"]);

      // Second tick updates session a in place; c stays cached.
      answer(sid, { ok: true, answer: sessionPage([session("a", 99), session("b", 20)], 3, "c1") });
      await drain();
      const last = results[results.length - 1];
      expect(idsOf(last.rows)).toEqual(["a", "b", "c"]);
      expect((last.rows[0] as any).lastAtMs).toBe(99);
    } finally {
      cleanup();
    }
  });

  it("rewalks on total change or 60s rest interval", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      CS.subscribeAll("sessionUsage", {}, 15, (r) => results.push(r), { limit: 1, restEveryS: 60 });
      await drain();
      const sid = subId(posted)!;
      answer(sid, { ok: true, answer: sessionPage([session("a", 1)], 1) });
      await drain();
      expect(results[results.length - 1].complete).toBe(true);

      // Total change triggers rest walk.
      answer(sid, { ok: true, answer: sessionPage([session("a", 1)], 2, "c1") });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("b", 1)], 2) });
      await drain();
      expect(results[results.length - 1].complete).toBe(true);
      expect(results[results.length - 1].rows).toHaveLength(2);

      // 60s passes; next tick triggers a NEW rest walk.
      await vi.advanceTimersByTimeAsync(60000);
      answer(sid, { ok: true, answer: sessionPage([session("a", 1)], 2, "c1") });
      await drain();
      const qid = lastQueryId(posted);
      expect(qid).toBeDefined();
      answer(qid!, { ok: true, answer: sessionPage([session("b", 1)], 2) });
      await drain();
      expect(results[results.length - 1].rows).toHaveLength(2);
    } finally {
      cleanup();
    }
  });

  it("coalesces the newest tick while a continuation is pending", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      CS.subscribeAll("sessionUsage", {}, 15, (r) => results.push(r), { limit: 1 });
      await drain();
      const sid = subId(posted)!;
      // First tick starts full walk.
      answer(sid, { ok: true, answer: sessionPage([session("a", 1)], 3, "c1") });
      await drain();
      const q1 = lastQueryId(posted)!;
      // First continuation answers with progress; q2 is now pending.
      answer(q1, { ok: true, answer: sessionPage([session("b", 1)], 3, "c2") });
      await drain();
      // New tick arrives while q2 is pending.
      answer(sid, { ok: true, answer: sessionPage([session("a2", 2)], 3, "c1") });
      await drain();
      // Missing from a head is not deletion. The old reply may add c, then
      // an actual fresh traversal must establish the new membership.
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("c", 1)], 3) });
      await drain();
      expect(idsOf(results[results.length - 1].rows)).toContain("a");
      // The 15-second head did not trigger another history walk. Reconcile
      // only when the rest interval permits a fresh full traversal.
      await vi.advanceTimersByTimeAsync(60000);
      answer(sid, { ok: true, answer: sessionPage([session("a2", 2)], 3, "new-c1") });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: sessionPage([session("a2", 2)], 3, "new-c1"),
      });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: sessionPage([session("b", 1)], 3, "new-c2"),
      });
      await drain();
      answer(lastQueryId(posted)!, { ok: true, answer: sessionPage([session("c", 1)], 3) });
      await drain();
      const last = results[results.length - 1];
      expect(idsOf(last.rows).sort()).toEqual(["a2", "b", "c"]);
      expect(last.complete).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("reconciles same-total membership replacement on a full refresh", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      CS.subscribeAll("queryContext", {}, 15, (r) => results.push(r), { limit: 2, restEveryS: 60 });
      await drain();
      const sid = subId(posted)!;
      answer(sid, { ok: true, answer: contextPage([{ id: "a" }, { id: "b" }], 4, "c1") });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: contextPage([{ id: "c" }, { id: "d" }], 4),
      });
      await drain();
      expect(idsOf(results[results.length - 1].rows).sort()).toEqual(["a", "b", "c", "d"]);

      // 60s later a new full refresh with different members, same total.
      await vi.advanceTimersByTimeAsync(60000);
      answer(sid, { ok: true, answer: contextPage([{ id: "e" }, { id: "f" }], 4, "c1") });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: contextPage([{ id: "g" }, { id: "h" }], 4),
      });
      await drain();
      const last = results[results.length - 1];
      expect(idsOf(last.rows).sort()).toEqual(["e", "f", "g", "h"]);
      expect(last.total).toBe(4);
    } finally {
      cleanup();
    }
  });

  it("reconciles terminal shrink when the window loses its continuation", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      CS.subscribeAll("queryContext", {}, 15, (r) => results.push(r), { limit: 2, restEveryS: 60 });
      await drain();
      const sid = subId(posted)!;
      answer(sid, { ok: true, answer: contextPage([{ id: "a" }, { id: "b" }], 4, "c1") });
      await drain();
      answer(lastQueryId(posted)!, {
        ok: true,
        answer: contextPage([{ id: "c" }, { id: "d" }], 4),
      });
      await drain();

      // 60s later total shrinks and page 1 has no cursor.
      await vi.advanceTimersByTimeAsync(60000);
      answer(sid, { ok: true, answer: contextPage([{ id: "e" }, { id: "f" }], 2) });
      await drain();
      const last = results[results.length - 1];
      expect(idsOf(last.rows).sort()).toEqual(["e", "f"]);
      expect(last.total).toBe(2);
      expect(last.complete).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("stop cancels pending retry asks and suppresses late results", async () => {
    const { CS, posted, answer, cleanup } = loadToolkit();
    try {
      const results: Result[] = [];
      const stop = CS.subscribeAll("sessionUsage", {}, 15, (r) => results.push(r), { limit: 1 });
      await drain();
      const sid = subId(posted)!;
      answer(sid, { ok: true, answer: sessionPage([session("a", 1)], 2, "c1") });
      await drain();
      const before = queryIds(posted).length;
      const callsBeforeStop = results.length;
      answer(lastQueryId(posted)!, { ok: false, error: "rate_limited" });
      await drain();
      stop();
      // No retry after stop.
      await vi.advanceTimersByTimeAsync(30000);
      expect(queryIds(posted).length).toBe(before);
      expect(results.length).toBe(callsBeforeStop);
      // Late subscription answer suppressed.
      answer(sid, { ok: true, answer: sessionPage([session("z", 9)], 2, "c1") });
      await drain();
      expect(results.length).toBe(callsBeforeStop);
    } finally {
      cleanup();
    }
  });
});

// --- partialList ---

describe("CS.partialList", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("renders the approved status states and Load more action", async () => {
    const { CS, cleanup } = loadToolkit();
    try {
      const container = document.createElement("div");
      let cb: (r: Result) => void = () => {};
      let loadMoreCalls = 0;
      CS.partialList({
        container,
        subscribe: (fn: (r: Result) => void) => {
          cb = fn;
          return () => {};
        },
        loadMore: () => {
          loadMoreCalls++;
        },
        render: (row: any) => {
          const el = document.createElement("div");
          el.textContent = row.id;
          return el;
        },
      });
      cb({
        rows: [{ id: "a" }, { id: "b" }],
        total: 5,
        complete: false,
        truncated: true,
        returned: 2,
        loading: false,
      });
      expect(container.querySelector(".cs-partial-list-text")?.textContent).toBe("Showing 2 of 5");
      const more = container.querySelector(".cs-partial-list-more") as HTMLButtonElement;
      more.click();
      await drain();
      expect(loadMoreCalls).toBe(1);

      cb({
        rows: [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }, { id: "e" }],
        total: 5,
        complete: true,
        truncated: false,
        returned: 5,
        loading: false,
      });
      expect(container.querySelector(".cs-partial-list-text")?.textContent).toBe("Showing all 5");
      expect(more.hidden).toBe(true);
    } finally {
      cleanup();
    }
  });

  it("shows Latest N of M when recovery budget is exhausted", async () => {
    const { CS, cleanup } = loadToolkit();
    try {
      const container = document.createElement("div");
      let cb: (r: Result) => void = () => {};
      CS.partialList({
        container,
        subscribe: (fn: (r: Result) => void) => {
          cb = fn;
          return () => {};
        },
        render: (row: any) => {
          const el = document.createElement("div");
          el.textContent = row.id;
          return el;
        },
      });
      cb({
        rows: [{ id: "a" }, { id: "b" }],
        total: 5,
        complete: false,
        truncated: false,
        returned: 2,
        loading: false,
      });
      expect(container.querySelector(".cs-partial-list-text")?.textContent).toBe("Latest 2 of 5");
    } finally {
      cleanup();
    }
  });
});

describe.each(["sessionUsage", "queryContext"])("hosted lifecycle: %s", (name) => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  const row = (id: string, value = 1) =>
    name === "sessionUsage"
      ? { cli: "codex", sessionId: id, machineId: "mac", value }
      : { path: id, value };
  const page = (rows: unknown[], total?: number, cursor?: string, cost = 1) => ({
    [name === "sessionUsage" ? "sessions" : "rows"]: rows,
    ...(total === undefined ? {} : { total }),
    nextCursor: cursor,
    totals: { cost },
    days: [{ cost }],
  });

  function reviewFixture(opts: Record<string, unknown>) {
    const h = loadToolkit();
    const results: Result[] = [];
    const stop = h.CS.subscribeAll(name, {}, 15, (r) => results.push(r), opts);
    return {
      stop,
      results,
      hosted: true,
      cleanup: h.cleanup,
      head: async (
        answer: unknown,
        error?: Error,
        outer: { source?: string; answered_at?: number; server_time?: number } = {},
      ) => {
        h.answer(
          subId(h.posted)!,
          error ? { ok: false, error: error.message } : { ok: true, answer, ...outer },
        );
        await drain();
      },
      reply: (answer: unknown) => h.answer(lastQueryId(h.posted), { ok: true, answer }),
      fail: (error: string) => h.answer(lastQueryId(h.posted), { ok: false, error }),
      count: () => queryIds(h.posted).length,
      args: () => h.posted.filter((m) => m.op === "query").at(-1)!.args as Record<string, unknown>,
    };
  }

  it("prepares queued reconciliation before an internal stale restart and skips an already complete head", async () => {
    for (const terminal of [false, true]) {
      const h = reviewFixture({ limit: 1 });
      try {
        await h.head(page([row("a")], 2, "old", 1));
        expect(h.count()).toBe(1);
        await h.head(terminal ? page([], 0, undefined, 0) : page([row("a", 9)], 3, "queued", 9));
        h.fail("query_invalid");
        await drain();
        if (terminal) {
          expect(h.results.at(-1)).toMatchObject({
            rows: [],
            returned: 0,
            total: 0,
            complete: true,
            canLoadMore: false,
            loading: false,
            totals: { cost: 0 },
            days: [{ cost: 0 }],
          });
          expect(h.count()).toBe(1);
          continue;
        }
        expect(h.count()).toBe(2);
        expect(h.args()).toMatchObject({ limit: 1, cursor: "queued" });
        h.reply(page([row("a", 11), row("b"), row("c")], 4, "new-tail", 11));
        await drain();
        expect(h.results.at(-1)).toMatchObject({
          returned: 3,
          total: 4,
          totals: { cost: 11 },
          days: [{ cost: 11 }],
        });
        expect(h.results.at(-1)?.rows).toContainEqual(row("a", 11));
        expect(h.count()).toBe(3);
        expect(h.args()).toMatchObject({ limit: 1, cursor: "new-tail" });
        h.reply(page([row("d")], 4, undefined, 11));
        await drain();
        expect(h.results.at(-1)).toMatchObject({
          returned: 4,
          total: 4,
          complete: true,
          canLoadMore: false,
          loading: false,
          totals: { cost: 11 },
          days: [{ cost: 11 }],
        });
        expect(h.results.at(-1)?.rows).toContainEqual(row("a", 11));
        await drain();
        expect(h.count()).toBe(3);
      } finally {
        h.stop();
        h.cleanup();
      }
    }
  });

  it("reconciles a queued head before an ordinary failure retry accepts newer data", async () => {
    const h = reviewFixture({ limit: 1 });
    try {
      await h.head(page([row("a")], 2, "old", 1));
      await h.head(page([row("a", 9)], 3, "queued", 9));
      h.fail("offline");
      await drain();
      expect(h.results.at(-1)?.error?.message).toBe("offline");
      const retry = h.stop.loadMore();
      expect(h.args().cursor).toBe("queued");
      h.reply(page([row("a", 11), row("b"), row("c")], 4, "new-tail", 11));
      const result = await retry;
      expect(result).toMatchObject({
        returned: 3,
        total: 4,
        complete: false,
        canLoadMore: true,
        totals: { cost: 11 },
        days: [{ cost: 11 }],
        error: null,
      });
      expect(result.rows).toContainEqual(row("a", 11));
      const count = h.count();
      await drain();
      expect(h.count()).toBe(count);
      const last = h.stop.loadMore();
      expect(h.args().cursor).toBe("new-tail");
      h.reply(page([row("d")], 4, undefined, 11));
      expect(await last).toMatchObject({
        returned: 4,
        total: 4,
        complete: true,
        canLoadMore: false,
        totals: { cost: 11 },
        days: [{ cost: 11 }],
      });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("retains a head warning through page success, next page failure and retry until an identical successful head", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      const initial = page([row("a")], 4, "c1", 1);
      await h.head(initial, undefined, { source: "live", answered_at: 100, server_time: 101 });
      const next = h.stop.loadMore();
      await h.head(null, new Error("rate_limited"));
      expect(h.results.at(-1)).toMatchObject({ loading: true, error: { message: "rate_limited" } });
      h.reply(page([row("b")], 4, "c2", 2));
      expect(await next).toMatchObject({
        returned: 2,
        totals: { cost: 2 },
        error: { message: "rate_limited" },
      });
      const failed = h.stop.loadMore();
      expect(h.results.at(-1)).toMatchObject({ loading: true, error: { message: "rate_limited" } });
      const rejection = expect(failed).rejects.toThrow("offline");
      h.fail("offline");
      await rejection;
      expect(h.results.at(-1)?.error?.message).toBe("offline");
      const retry = h.stop.loadMore();
      expect(h.args().cursor).toBe("c2");
      expect(h.results.at(-1)?.error?.message).toBe("rate_limited");
      h.reply(page([row("c"), row("d")], 4, undefined, 4));
      expect(await retry).toMatchObject({
        complete: true,
        returned: 4,
        totals: { cost: 4 },
        days: [{ cost: 4 }],
        error: { message: "rate_limited" },
      });
      if (h.hosted)
        expect(h.results.at(-1)).toMatchObject({
          source: "live",
          answered_at: 100,
          server_time: 101,
        });
      const count = h.count();
      await h.head(initial, undefined, { source: "cache", answered_at: 900, server_time: 901 });
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        returned: 4,
        totals: { cost: 4 },
        error: null,
      });
      if (h.hosted)
        expect(h.results.at(-1)).toMatchObject({
          source: "cache",
          answered_at: 900,
          server_time: 901,
        });
      expect(h.count()).toBe(count);
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("enforces the public hard 5000-row ceiling with a larger requested cap and ordinary pages", async () => {
    const h = reviewFixture({ manual: true, limit: 100, maxRows: 9000 });
    const rows = Array.from({ length: 5101 }, (_, i) => row(String(i)));
    try {
      await h.head(page(rows.slice(0, 100), rows.length, "100"));
      for (let offset = 100; offset < 5000; offset += 100) {
        const next = h.stop.loadMore();
        expect(h.args()).toMatchObject({ limit: 100, cursor: String(offset) });
        h.reply(page(rows.slice(offset, offset + 100), rows.length, String(offset + 100)));
        await next;
      }
      const result = h.results.at(-1)!;
      expect(result).toMatchObject({
        returned: 5000,
        total: 5101,
        atCeiling: true,
        complete: false,
        truncated: true,
        canLoadMore: false,
      });
      expect(result.rows).toEqual(rows.slice(0, 5000));
      expect(new Set(result.rows.map((r) => JSON.stringify(r))).size).toBe(5000);
      expect(h.count()).toBe(49);
      await h.stop.loadMore();
      await drain();
      expect(h.count()).toBe(49);
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("completes a fenced final manual page with the newest rows and full summary", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      await h.head(page([row("a")], 2, "c1"));
      const final = h.stop.loadMore();
      await h.head(page([row("a", 9)], 2, "head", 9), undefined, {
        source: "cache",
        answered_at: 900,
        server_time: 901,
      });
      const count = h.count();
      h.reply(page([row("a", 2), row("b")], 99, undefined, 2));
      const result = await final;
      expect(result).toMatchObject({
        complete: true,
        returned: 2,
        total: 2,
        canLoadMore: false,
        totals: { cost: 9 },
        days: [{ cost: 9 }],
      });
      expect(result.rows).toContainEqual(row("a", 9));
      if (h.hosted)
        expect(result).toMatchObject({ source: "cache", answered_at: 900, server_time: 901 });
      await drain();
      expect(h.count()).toBe(count);
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("keeps completed manual depth after rest and fully head-supplied growth", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      await h.head(page([row("a")], 2, "c1"));
      const final = h.stop.loadMore();
      h.reply(page([row("b")], 2));
      await final;
      vi.setSystemTime(Date.now() + 60000);
      const count = h.count();
      await h.head(page([row("a", 9)], 2, "head", 9));
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        returned: 2,
        canLoadMore: false,
        totals: { cost: 9 },
      });
      await h.head(page([row("a", 10), row("c")], 3, "growth", 10));
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        returned: 3,
        total: 3,
        canLoadMore: false,
        totals: { cost: 10 },
      });
      expect(h.count()).toBe(count);
      await h.head(page([row("a", 11)], 4, "missing", 11));
      expect(h.results.at(-1)).toMatchObject({ complete: false, returned: 3, canLoadMore: true });
      expect(h.count()).toBe(count);
      const next = h.stop.loadMore();
      expect(h.args().cursor).toBe("missing");
      h.reply(page([row("d")], 4, undefined, 11));
      expect(await next).toMatchObject({ complete: true, returned: 4 });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("coalesces the newest automatic head during a rate-limited pending continuation", async () => {
    const h = reviewFixture({ limit: 1 });
    try {
      await h.head(page([row("a")], 2, "old"));
      h.fail("rate_limited");
      await drain();
      await h.head(page([row("a", 5)], 3, "h1", 5));
      await h.head(page([row("a", 9)], 3, "h2", 9), undefined, {
        source: "cache",
        answered_at: 900,
        server_time: 901,
      });
      await vi.advanceTimersByTimeAsync(5000);
      expect(h.args().cursor).toBe("old");
      h.reply(page([row("a", 2), row("b")], 2, "old-tail", 2));
      await drain();
      expect(h.args().cursor).toBe("h2");
      if (h.hosted)
        expect(h.results.at(-1)).toMatchObject({
          source: "cache",
          answered_at: 900,
          server_time: 901,
        });
      expect(h.results.at(-1)).toMatchObject({
        total: 3,
        totals: { cost: 9 },
        days: [{ cost: 9 }],
      });
      expect(h.results.at(-1)?.rows).toContainEqual(row("a", 9));
      h.reply(page([row("b"), row("c")], 3, undefined, 9));
      await drain();
      expect(h.results.at(-1)).toMatchObject({
        complete: true,
        total: 3,
        returned: 3,
        totals: { cost: 9 },
      });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("waits for a successful head after each invalid manual cursor before explicit continuation", async () => {
    const h = reviewFixture({ manual: true, limit: 1 });
    try {
      const old = page([row("a")], 2, "c1", 9);
      await h.head(old);
      for (let attempt = 0; attempt < 2; attempt++) {
        const next = h.stop.loadMore();
        const failed = expect(next).rejects.toThrow("query_invalid");
        h.fail("query_invalid");
        await failed;
        const count = h.count();
        expect(h.results.at(-1)).toMatchObject({
          returned: 1,
          total: 2,
          canLoadMore: false,
          waitingForHead: true,
          totals: { cost: 9 },
        });
        await h.stop.loadMore();
        await h.stop.refresh();
        expect(h.count()).toBe(count);
        await h.head(null, new Error("mac_offline"));
        await h.stop.refresh();
        expect(h.results.at(-1)).toMatchObject({ waitingForHead: true, canLoadMore: false });
        expect(h.count()).toBe(count);
        await h.head(old);
        expect(h.results.at(-1)).toMatchObject({
          waitingForHead: false,
          canLoadMore: true,
          error: null,
        });
        expect(h.count()).toBe(count);
      }
      const final = h.stop.loadMore();
      expect(h.args().cursor).toBe("c1");
      h.reply(page([row("b")], 2, undefined, 9));
      expect(await final).toMatchObject({ complete: true, returned: 2 });
    } finally {
      h.stop();
      h.cleanup();
    }
  });

  it("preserves newer page data on identical old heads, retaining actual outer freshness", async () => {
    const h = loadToolkit();
    const results: Result[] = [];
    const stop = h.CS.subscribeAll(name, {}, 15, (r) => results.push(r), {
      manual: true,
      limit: 1,
    });
    try {
      const sid = subId(h.posted)!;
      const old = page([row("a")], 300, "c1", 1);
      h.answer(sid, { ok: true, answer: old, source: "live", answered_at: 100, server_time: 101 });
      const p = stop.loadMore();
      h.answer(lastQueryId(h.posted), {
        ok: true,
        answer: page([row("a", 9), row("b")], 301, undefined, 88.88),
      });
      await p;
      h.answer(sid, { ok: false, error: "rate_limited" });
      expect(results.at(-1)?.error?.message).toBe("rate_limited");
      h.answer(sid, { ok: true, answer: old, source: "cache", answered_at: 100, server_time: 200 });
      expect(results.at(-1)?.error).toBeNull();
      expect(results.at(-1)).toMatchObject({
        total: 301,
        returned: 2,
        totals: { cost: 88.88 },
        days: [{ cost: 88.88 }],
        source: "cache",
        answered_at: 100,
        server_time: 200,
        canLoadMore: true,
      });
      expect(results.at(-1)?.rows).toContainEqual(row("a", 9));
    } finally {
      stop();
      h.cleanup();
    }
    for (const total of [undefined, 1]) {
      const h = loadToolkit();
      const results: Result[] = [];
      const stop = h.CS.subscribeAll(name, {}, 15, (r) => results.push(r), {
        manual: true,
        limit: 1,
      });
      try {
        const sid = subId(h.posted)!;
        const old = page([row("a")], total, "c1");
        h.answer(sid, { ok: true, answer: old });
        const p = stop.loadMore();
        h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("b")], total) });
        await p;
        const count = queryIds(h.posted).length;
        h.answer(sid, { ok: true, answer: old });
        expect(results.at(-1)).toMatchObject({ complete: false, canLoadMore: false });
        expect(queryIds(h.posted)).toHaveLength(count);
      } finally {
        stop();
        h.cleanup();
      }
    }
  });

  it("reports unknown, zero and overshoot honestly, accepting the latest summary", async () => {
    const h = loadToolkit();
    try {
      const unknown = h.CS.queryAll(name, {});
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("a")]) });
      expect(await unknown).toMatchObject({ total: null, complete: false, returned: 1 });
      const zero = h.CS.queryAll(name, {});
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([], 0) });
      expect(await zero).toMatchObject({ total: 0, complete: true, returned: 0 });
      const p = h.CS.queryAll(name, {}, { maxRows: 2 });
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("a")], 3, "c", 1) });
      await drain();
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("b")], 1, undefined, 9) });
      expect(await p).toMatchObject({
        total: 1,
        complete: false,
        returned: 2,
        totals: { cost: 9 },
        days: [{ cost: 9 }],
      });
    } finally {
      h.cleanup();
    }
  });

  it("captures original filters and limit, supports exactly one page per click and retains depth", async () => {
    const h = loadToolkit();
    const results: Result[] = [];
    const args = { filters: [{ value: "original" }], sinceMs: 123, limit: 2 };
    const stop = h.CS.subscribeAll(name, args, 17, (r) => results.push(r), {
      manual: true,
      limit: 1,
    });
    try {
      args.filters[0].value = "mutated";
      const sid = subId(h.posted)!;
      expect(h.posted.find((m) => m.op === "subscribe")).toMatchObject({
        every_s: 17,
        v: 1,
        args: { limit: 1, sinceMs: 123, filters: [{ value: "original" }] },
      });
      h.answer(sid, { ok: true, answer: page([row("a")], 4, "c1") });
      await drain();
      expect(queryIds(h.posted)).toHaveLength(0);
      expect(results.at(-1)).toMatchObject({ loading: false, returned: 1 });
      const one = stop.loadMore();
      const same = stop.loadMore();
      expect(same).toBe(one);
      expect(queryIds(h.posted)).toHaveLength(1);
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("b")], 4, "c2") });
      await one;
      h.answer(sid, { ok: true, answer: page([row("a", 9)], 5, "head") });
      await drain();
      expect(queryIds(h.posted)).toHaveLength(1);
      expect(results.at(-1)).toMatchObject({ returned: 2, total: 5 });
      const two = stop.loadMore();
      expect(h.posted.filter((m) => m.op === "query").at(-1)?.args).toEqual({
        filters: [{ value: "original" }],
        sinceMs: 123,
        limit: 1,
        cursor: "c2",
      });
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("c")], 4, "c3") });
      expect(await two).toMatchObject({ returned: 3 });
      expect(queryIds(h.posted)).toHaveLength(2);
    } finally {
      stop();
      h.cleanup();
    }
  });

  it("fences an older page while retaining its unseen rows and deepest cursor", async () => {
    const h = loadToolkit();
    const results: Result[] = [];
    const stop = h.CS.subscribeAll(name, {}, 15, (r) => results.push(r), {
      manual: true,
      limit: 1,
    });
    try {
      const sid = subId(h.posted)!;
      h.answer(sid, { ok: true, answer: page([row("a")], 4, "c1") });
      const p = stop.loadMore();
      const oldId = lastQueryId(h.posted);
      h.answer(sid, { ok: true, answer: page([row("a", 9)], 4, "head", 9) });
      h.answer(oldId, { ok: true, answer: page([row("a", 2), row("b")], 99, "c2", 2) });
      const r = await p;
      expect(r).toMatchObject({ total: 4, returned: 2, totals: { cost: 9 }, days: [{ cost: 9 }] });
      expect(r.rows).toContainEqual(row("a", 9));
      const next = stop.loadMore();
      expect(h.posted.filter((m) => m.op === "query").at(-1)?.args).toMatchObject({ cursor: "c2" });
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("c")], 4, "c3") });
      await next;
    } finally {
      stop();
      h.cleanup();
    }
  });

  it("reports initial and page errors, retries the same cursor and settles stop", async () => {
    const h = loadToolkit();
    const results: Result[] = [];
    const stop = h.CS.subscribeAll(name, {}, 15, (r) => results.push(r), {
      manual: true,
      limit: 1,
    });
    try {
      const sid = subId(h.posted)!;
      h.answer(sid, { ok: false, error: "not_allowed" });
      expect(results.at(-1)?.error?.message).toBe("not_allowed");
      h.answer(sid, { ok: true, answer: page([row("a")], 3, "c1") });
      const failed = stop.loadMore();
      const rejection = expect(failed).rejects.toThrow("offline");
      h.answer(lastQueryId(h.posted), { ok: false, error: "offline" });
      await rejection;
      expect(results.at(-1)).toMatchObject({ returned: 1, loading: false });
      expect(results.at(-1)?.error?.message).toBe("offline");
      const retry = stop.loadMore();
      expect(h.posted.filter((m) => m.op === "query").at(-1)?.args).toMatchObject({
        cursor: "c1",
        limit: 1,
      });
      const stopped = expect(retry).rejects.toThrow("stopped");
      const count = results.length;
      stop();
      await stopped;
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("b")], 3, "c2") });
      await drain();
      expect(results).toHaveLength(count);
    } finally {
      stop();
      h.cleanup();
    }
  });

  it("keeps capped visible membership until a fresh smaller traversal completes", async () => {
    const h = loadToolkit();
    const results: Result[] = [];
    const stop = h.CS.subscribeAll(name, {}, 15, (r) => results.push(r), { limit: 1, maxRows: 3 });
    try {
      const sid = subId(h.posted)!;
      h.answer(sid, { ok: true, answer: page([row("a"), row("b"), row("c")], 8, "old") });
      await drain();
      expect(results.at(-1)).toMatchObject({ returned: 3, complete: false });
      h.answer(sid, { ok: true, answer: page([row("x")], 2, "new") });
      await drain();
      expect(results.at(-1)?.rows).toEqual([row("a"), row("b"), row("c")]);
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("y")], 2) });
      await drain();
      expect(results.at(-1)).toMatchObject({
        rows: [row("x"), row("y")],
        returned: 2,
        complete: true,
      });
    } finally {
      stop();
      h.cleanup();
    }
  });

  it("keeps healthy heads cheap until the rest interval and supports explicit refresh", async () => {
    const h = loadToolkit();
    const stop = h.CS.subscribeAll(name, {}, 15, () => {}, { limit: 1, restEveryS: 60 });
    try {
      const sid = subId(h.posted)!;
      h.answer(sid, { ok: true, answer: page([row("a")], 2, "c1") });
      await drain();
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("b")], 2) });
      await drain();
      for (let i = 0; i < 3; i++) {
        await vi.advanceTimersByTimeAsync(15000);
        h.answer(sid, { ok: true, answer: page([row("a", i)], 2, "head") });
        await drain();
      }
      expect(queryIds(h.posted)).toHaveLength(1);
      await vi.advanceTimersByTimeAsync(15000);
      h.answer(sid, { ok: true, answer: page([row("a")], 2, "rest") });
      await drain();
      expect(queryIds(h.posted)).toHaveLength(2);
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([row("b")], 2) });
      await drain();
      const refresh = stop.refresh();
      expect(h.posted.filter((m) => m.op === "query").at(-1)?.args).toEqual({ limit: 1 });
      h.answer(lastQueryId(h.posted), { ok: true, answer: page([], 0) });
      expect(await refresh).toMatchObject({ complete: true, returned: 0 });
    } finally {
      stop();
      h.cleanup();
    }
  });
});

describe("hosted identity", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });
  it("uses exact CLI/session pairs and updates machine metadata", async () => {
    const h = loadToolkit();
    try {
      const p = h.CS.queryAll("sessionUsage", {}, { limit: 2 });
      h.answer(lastQueryId(h.posted), {
        ok: true,
        answer: sessionPage(
          [
            { cli: "codex", sessionId: "same", machineId: "one" },
            { cli: "claude", sessionId: "same", machineId: "one" },
          ],
          2,
          "c",
        ),
      });
      await drain();
      h.answer(lastQueryId(h.posted), {
        ok: true,
        answer: sessionPage([{ cli: "codex", sessionId: "same", machineId: "two" }], 2),
      });
      expect(await p).toMatchObject({
        complete: true,
        returned: 2,
        rows: [
          { cli: "codex", sessionId: "same", machineId: "two" },
          { cli: "claude", sessionId: "same", machineId: "one" },
        ],
      });
    } finally {
      h.cleanup();
    }
  });
});

function listFixture() {
  const h = loadToolkit();
  return {
    sdk: h.CS,
    head: (answer: unknown, error?: Error) =>
      h.answer(
        subId(h.posted)!,
        error ? { ok: false, error: error.message } : { ok: true, answer },
      ),
    reply: (answer: unknown) => h.answer(lastQueryId(h.posted), { ok: true, answer }),
    fail: (error: string) => h.answer(lastQueryId(h.posted), { ok: false, error }),
    count: () => queryIds(h.posted).length,
    args: () => h.posted.filter((m) => m.op === "query").at(-1)?.args as Record<string, unknown>,
    cleanup: h.cleanup,
  };
}

describe("shared partialList integration", () => {
  it("installs one synchronous default-only inline baseline, independent of terminal and identical across ports", async () => {
    document.getElementById("cs-partial-list-style")?.remove();
    const authored = document.createElement("style");
    authored.textContent = ".authored { color: red; }";
    document.head.append(authored);
    const h = listFixture();
    const container = document.createElement("div");
    let receive: (r: Result) => void = () => {};
    const loadMore = vi.fn(async () => ({
      rows: [],
      total: 0,
      returned: 0,
      complete: true,
      truncated: false,
    }));
    const beforeStyles = document.head.querySelectorAll("style").length;
    const view = h.sdk.partialList({
      container,
      render: () => document.createElement("div"),
      loadMore,
      subscribe: (cb: (r: Result) => void) => {
        expect(document.getElementById("cs-partial-list-style")).not.toBeNull();
        receive = cb;
        return () => {};
      },
    });
    const other = h.sdk.partialList({ container, render: () => document.createElement("div") });
    try {
      const resource = document.getElementById("cs-partial-list-style") as HTMLStyleElement;
      expect(document.head.firstChild).toBe(resource);
      expect(document.head.querySelectorAll("#cs-partial-list-style")).toHaveLength(1);
      expect(document.head.querySelectorAll("style")).toHaveLength(beforeStyles + 1);
      const rules = Array.from(resource.sheet!.cssRules) as CSSStyleRule[];
      expect(rules.length).toBeGreaterThan(0);
      expect(rules.every((rule) => rule.selectorText.includes(".cs-partial-list"))).toBe(true);
      const selectors = rules.map((rule) => rule.selectorText);
      for (const selector of [
        ":where(.cs-partial-list)",
        ":where(.cs-partial-list > .cs-partial-list-status)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:hover)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:focus-visible)",
        ":where(.cs-partial-list > .cs-partial-list-status > .cs-partial-list-more:disabled)",
        ":where(.cs-partial-list > .cs-partial-list-status) > .cs-partial-list-more[hidden]",
      ])
        expect(selectors).toContain(selector);
      const css = resource.textContent!;
      // Every variable use has a fallback, including nested hover foreground.
      for (const match of css.matchAll(/var\((--cs-[\w-]+)([^,)]*)/g)) expect(match[2]).toBe("");
      expect(css.match(/var\(--cs-[\w-]+,/g)?.length).toBe(css.match(/var\(/g)?.length);
      expect(css).not.toMatch(/--cs-[\w-]+\s*:/);
      const kitDir = join(process.cwd(), "src-tauri/src/http/routes/dashboard_kit");
      const tokens = readFileSync(join(kitDir, "tokens.css"), "utf8");
      for (const colors of css.matchAll(/light-dark\([^)]+\)/g))
        expect(tokens).toContain(colors[0]);
      const sources = [
        readFileSync(
          join(process.cwd(), "src-tauri/src/dashboard_publish/toolkit/cs-live.js"),
          "utf8",
        ),
        readFileSync(join(kitDir, "kit.js"), "utf8"),
      ];
      const resourceBlock = (s: string) =>
        s.slice(s.indexOf("  var PARTIAL_LIST_STYLE ="), s.indexOf("  function partialList("));
      const component = (s: string) => {
        const start = s.indexOf("  function partialList(");
        return s.slice(start, s.indexOf("\n  }\n", start) + 5);
      };
      expect(resourceBlock(sources[0])).toBe(resourceBlock(sources[1]));
      expect(component(sources[0])).toBe(component(sources[1]));
      const button = view.el.querySelector("button")!;
      receive({
        rows: [],
        returned: 0,
        total: 1,
        complete: false,
        truncated: true,
        canLoadMore: true,
        loading: true,
      });
      expect(button.disabled).toBe(true);
      button.click();
      expect(loadMore).not.toHaveBeenCalled();
      receive({
        rows: [],
        returned: 0,
        total: 1,
        complete: false,
        truncated: true,
        canLoadMore: true,
        loading: false,
      });
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(loadMore).toHaveBeenCalledOnce();
      expect(button.hidden).toBe(true);
      view.stop();
      other.stop();
      expect(document.getElementById("cs-partial-list-style")).toBe(resource);
    } finally {
      view.stop();
      other.stop();
      h.cleanup();
      authored.remove();
      document.getElementById("cs-partial-list-style")?.remove();
    }
  });

  it("leaves bound controls authored and outside every default baseline selector", () => {
    document.getElementById("cs-partial-list-style")?.remove();
    const h = listFixture();
    const layout = document.createElement("section");
    const status = document.createElement("p");
    const button = document.createElement("button");
    status.className = "authored-status";
    button.className = "authored-button";
    status.style.color = "red";
    button.style.margin = "3px";
    layout.append(status, button);
    const attributes = (el: Element) => [el.className, el.getAttribute("style")];
    const before = [attributes(status), attributes(button)];
    const bound = h.sdk.partialList({ status, button });
    let generated: ReturnType<typeof h.sdk.partialList> | undefined;
    try {
      expect(document.getElementById("cs-partial-list-style")).toBeNull();
      expect(Array.from(layout.children)).toEqual([status, button]);
      expect([attributes(status), attributes(button)]).toEqual(before);
      generated = h.sdk.partialList({
        container: document.createElement("div"),
        render: () => document.createElement("div"),
      });
      const resource = document.getElementById("cs-partial-list-style") as HTMLStyleElement;
      const rules = Array.from(resource.sheet!.cssRules) as CSSStyleRule[];
      for (const rule of rules)
        for (const el of [layout, status, button])
          expect(el.matches(rule.selectorText)).toBe(false);
      expect([attributes(status), attributes(button)]).toEqual(before);
      expect(Array.from(layout.children)).toEqual([status, button]);
    } finally {
      bound.stop();
      generated?.stop();
      h.cleanup();
      document.getElementById("cs-partial-list-style")?.remove();
    }
  });

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("keeps an unresolved head warning in bound controls and actual onResult callbacks through page actions", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const results: Result[] = [];
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true }),
      onResult: (result: Result) => results.push(result),
    });
    try {
      const initial = { rows: [{ id: "a" }], total: 3, nextCursor: "c1", totals: { cost: 1 } };
      h.head(initial);
      const next = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      h.head(null, new Error("rate_limited"));
      const warned = results.length - 1;
      expect(results.at(-1)).toMatchObject({ loading: true, error: { message: "rate_limited" } });
      expect(status.textContent).toContain("rate_limited");
      h.reply({ rows: [{ id: "b" }], total: 3, nextCursor: "c2", totals: { cost: 2 } });
      await next;
      expect(results.at(-1)).toMatchObject({
        returned: 2,
        totals: { cost: 2 },
        error: { message: "rate_limited" },
      });
      expect(status.textContent).toContain("rate_limited");
      const failed = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(status.textContent).toContain("rate_limited");
      h.fail("offline");
      await failed;
      expect(status.textContent).toContain("offline");
      expect(button.textContent).toBe("Retry");
      const retry = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.args().cursor).toBe("c2");
      expect(status.textContent).toContain("rate_limited");
      h.reply({ rows: [{ id: "c" }], total: 3, totals: { cost: 3 } });
      await retry;
      expect(results.at(-1)).toMatchObject({
        complete: true,
        returned: 3,
        totals: { cost: 3 },
        error: { message: "rate_limited" },
      });
      expect(status.textContent).toContain("rate_limited");
      expect(results.slice(warned).every((r) => !!r.error)).toBe(true);
      const count = h.count();
      h.head(initial);
      await vi.advanceTimersByTimeAsync(0);
      expect(results.at(-1)).toMatchObject({
        complete: true,
        returned: 3,
        totals: { cost: 3 },
        error: null,
      });
      expect(status.textContent).toBe("Showing all 3");
      expect(h.count()).toBe(count);
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("keeps stale manual component controls waiting until a head, then needs a click", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true }),
    });
    try {
      const old = { rows: [{ id: "a" }], total: 2, nextCursor: "c1" };
      h.head(old);
      const next = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      h.fail("query_invalid");
      await next;
      const count = h.count();
      expect(status.textContent).toContain("Waiting for fresh data");
      expect(button.textContent).toBe("Waiting…");
      expect(button.disabled).toBe(true);
      expect(button.hidden).toBe(false);
      button.click();
      await view.refresh();
      await view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(count);
      h.head(old);
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(count);
      expect(button.disabled).toBe(false);
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(count + 1);
      expect(h.args().cursor).toBe("c1");
      h.reply({ rows: [{ id: "b" }], total: 2 });
      await vi.advanceTimersByTimeAsync(0);
      expect(status.textContent).toBe("Showing all 2");
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("settles its public query action on stop while the dispatched query remains pending", async () => {
    const h = listFixture();
    const container = document.createElement("div");
    let reply!: (result: Result) => void;
    const query = vi.fn(
      () =>
        new Promise<Result>((resolve) => {
          reply = resolve;
        }),
    );
    const render = vi.fn(() => document.createElement("div"));
    const view = h.sdk.partialList({ container, query, render });
    const action = view.refresh();
    let settled = false;
    action.then(() => {
      settled = true;
    });
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(query).toHaveBeenCalledOnce();
      expect(settled).toBe(false);
      const before = container.textContent;
      view.stop();
      await vi.advanceTimersByTimeAsync(0);
      expect(settled).toBe(true);
      reply({ rows: [{ id: "late" }], total: 1, returned: 1, complete: true, truncated: false });
      await vi.advanceTimersByTimeAsync(0);
      expect(render).not.toHaveBeenCalled();
      expect(container.textContent).toBe(before);
      expect(container.querySelector("button")!.disabled).toBe(true);
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("binds actual paging controls, handles retry/double click, refreshes zero and stops", async () => {
    const h = listFixture();
    const container = document.createElement("div");
    const view = h.sdk.partialList({
      container,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true, limit: 1 }),
      render: (row: any) => {
        const el = document.createElement("div");
        el.textContent = row.id;
        return el;
      },
    });
    try {
      h.head({ rows: [{ id: "a" }], nextCursor: "c1" });
      await vi.advanceTimersByTimeAsync(0);
      const text = container.querySelector(".cs-partial-list-text")!;
      const button = container.querySelector("button")!;
      expect(text.textContent).toBe("Loaded 1 · total unknown");
      button.click();
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(1);
      expect(h.args().cursor).toBe("c1");
      h.fail("offline");
      await vi.advanceTimersByTimeAsync(0);
      expect(text.textContent).toContain("offline");
      expect(button.textContent).toBe("Retry");
      button.click();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(2);
      expect(h.args().cursor).toBe("c1");
      h.reply({ rows: [{ id: "b" }], total: 2 });
      await vi.advanceTimersByTimeAsync(0);
      expect(text.textContent).toBe("Showing all 2");
      expect(button.hidden).toBe(true);
      const refresh = view.refresh();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.args().cursor).toBeUndefined();
      h.reply({ rows: [], total: 0 });
      await refresh;
      expect(text.textContent).toBe("Showing all 0");
      view.stop();
      h.head({ rows: [{ id: "late" }], total: 1 });
      expect(text.textContent).toBe("Showing all 0");
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("binds existing coverage elements without a duplicate list and reports the ceiling", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const layout = document.createElement("section");
    layout.append(status, button);
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true, maxRows: 1 }),
    });
    try {
      h.head({ rows: [{ id: "a" }], total: 3, nextCursor: "c" });
      await vi.advanceTimersByTimeAsync(0);
      expect(layout.children).toHaveLength(2);
      expect(status.textContent).toContain("Showing 1 of 3");
      expect(status.textContent).toContain("row limit reached");
      expect(button.hidden).toBe(true);
      expect(h.count()).toBe(0);
    } finally {
      view.stop();
      h.cleanup();
    }
  });

  it("settles an in-flight bound component action when stopped and suppresses late replies", async () => {
    const h = listFixture();
    const status = document.createElement("p");
    const button = document.createElement("button");
    const view = h.sdk.partialList({
      status,
      button,
      subscribe: (receive: (r: Result) => void) =>
        h.sdk.subscribeAll("queryContext", {}, 15, receive, { manual: true }),
    });
    try {
      h.head({ rows: [{ id: "a" }], total: 2, nextCursor: "c" });
      const loading = view.loadMore();
      await vi.advanceTimersByTimeAsync(0);
      expect(h.count()).toBe(1);
      view.stop();
      await loading;
      const text = status.textContent;
      h.reply({ rows: [{ id: "b" }], total: 2 });
      await vi.advanceTimersByTimeAsync(0);
      expect(status.textContent).toBe(text);
      expect(button.disabled).toBe(true);
    } finally {
      view.stop();
      h.cleanup();
    }
  });
});

it("surfaces arg_not_allowed on a paged query without restarting the head", async () => {
  vi.useFakeTimers();
  const h = loadToolkit();
  try {
    const pending = h.CS.queryAll("queryContext", {}, { limit: 1 });
    await drain();
    h.answerNext({ ok: true, answer: contextPage([{ id: "a" }], 2, "tail") });
    await drain();
    await drain();
    const rejected = expect(pending).rejects.toThrow("arg_not_allowed");
    h.answerNext({ ok: false, error: "arg_not_allowed" });
    await rejected;
    expect(queryIds(h.posted)).toHaveLength(2);
  } finally {
    h.cleanup();
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
  }
});

function anyListFixture() {
  const h = loadToolkit();
  return {
    sdk: h.CS,
    reply: (answer: unknown) => h.answer(lastQueryId(h.posted), { ok: true, answer }),
    fail: (error: string) => h.answer(lastQueryId(h.posted), { ok: false, error }),
    head: async (answer: unknown) => {
      const id = h.posted.filter((m) => m.op === "subscribe").at(-1)!.id as string;
      h.answer(id, { ok: true, answer });
      await vi.advanceTimersByTimeAsync(0);
    },
    args: () => h.posted.filter((m) => m.op === "query").at(-1)!.args as any,
    count: () => queryIds(h.posted).length,
    cleanup: h.cleanup,
  };
}

testAnyListPaging(anyListFixture);
