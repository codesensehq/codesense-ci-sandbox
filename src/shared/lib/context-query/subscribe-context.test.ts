// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ContextQuery, ContextQueryResult } from "./types";

const queryContext = vi.fn<(q: ContextQuery, page?: string) => Promise<ContextQueryResult>>();
vi.mock("@/tauri/commands", () => ({
  queryContext: (q: ContextQuery, page?: string) => queryContext(q, page),
}));

type Change = {
  orgs: string[];
  allOrgs: boolean;
  truncated: boolean;
  generation: number;
  paths: string[];
};
const handlers = new Set<(e: { payload: Change | null }) => void>();
const unlisten = vi.fn();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, handler: (e: { payload: Change | null }) => void) => {
    handlers.add(handler);
    return () => {
      handlers.delete(handler);
      unlisten();
    };
  }),
}));

import { subscribeContext } from "./subscribe-context";

function result(total: number): ContextQueryResult {
  return {
    schemaVersion: 1,
    total,
    rows: [],
    textScanTruncated: false,
    totalApproximate: false,
    indexing: false,
    generation: total,
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));
const indexChanged = (payload: Change | null = null) =>
  [...handlers].forEach((h) => h({ payload }));
const change = (orgs: string[], extra: Partial<Change> = {}): Change => ({
  orgs,
  allOrgs: false,
  truncated: false,
  generation: 1,
  paths: [],
  ...extra,
});

beforeEach(() => {
  queryContext.mockReset();
  handlers.clear();
  unlisten.mockClear();
});

describe("subscribeContext", () => {
  it("runs now, then again on every index change, with the same query", async () => {
    queryContext.mockResolvedValueOnce(result(1)).mockResolvedValueOnce(result(2));
    const seen: number[] = [];
    const q: ContextQuery = { where: { kinds: ["story"] } };
    const off = subscribeContext(q, (u) => u.ok && seen.push(u.result.total), "/p");
    await flush();
    indexChanged();
    await flush();
    expect(seen).toEqual([1, 2]);
    expect(queryContext).toHaveBeenCalledTimes(2);
    expect(queryContext.mock.calls.every(([arg]) => arg === q)).toBe(true);
    off();
  });

  it("an older, slower answer never overwrites a newer one", async () => {
    let resolveSlow: (r: ContextQueryResult) => void = () => {};
    queryContext
      .mockReturnValueOnce(new Promise((r) => (resolveSlow = r)))
      .mockResolvedValueOnce(result(2));
    const seen: number[] = [];
    const off = subscribeContext({}, (u) => u.ok && seen.push(u.result.total), "/p");
    await flush();
    indexChanged(); // newer query starts and lands first
    await flush();
    resolveSlow(result(1)); // the stale first answer arrives late
    await flush();
    expect(seen).toEqual([2]);
    off();
  });

  it("reports errors and shares one listener; the last unsubscribe detaches it", async () => {
    queryContext.mockRejectedValue(new Error("boom"));
    const errors: string[] = [];
    const a = subscribeContext({}, (u) => !u.ok && errors.push(u.error), "/p");
    const b = subscribeContext({}, () => {}, "/p");
    await flush();
    expect(errors).toEqual(["boom"]);
    expect(handlers.size).toBe(1);
    a();
    a(); // idempotent
    expect(unlisten).not.toHaveBeenCalled();
    b();
    expect(unlisten).toHaveBeenCalledTimes(1);
    // No callback after unsubscribe.
    indexChanged();
    await flush();
    expect(errors).toEqual(["boom"]);
  });

  it("re-runs only for changes in its scope (or all-orgs / replaced-index changes)", async () => {
    queryContext.mockResolvedValue({ ...result(1), scopeOrgs: ["acme"] });
    const off = subscribeContext({ where: { orgs: ["acme"] } }, () => {}, "/p/board.html");
    await flush();
    expect(queryContext).toHaveBeenCalledTimes(1);
    expect(queryContext.mock.calls[0][1]).toBe("/p/board.html");
    indexChanged(change(["globex"]));
    await flush();
    expect(queryContext).toHaveBeenCalledTimes(1);
    indexChanged(change(["acme"]));
    await flush();
    expect(queryContext).toHaveBeenCalledTimes(2);
    indexChanged(change([], { allOrgs: true }));
    await flush();
    expect(queryContext).toHaveBeenCalledTimes(3);
    indexChanged(change([], { truncated: true }));
    await flush();
    expect(queryContext).toHaveBeenCalledTimes(4);
    off();
  });
});
