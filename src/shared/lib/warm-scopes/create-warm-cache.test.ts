// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { createWarmCache } from "./create-warm-cache";
import { WarmScopes } from "./warm-scopes";

const A = "org1:a";
const B = "org1:b";
const C = "org2:c";
const D = "org1:d";
const E = "org2:e";
const scopeOf = (key: string) => key.split("/")[0];

function make() {
  return new WarmScopes({ warmCapacity: 3, suspendedAfterSwitches: 4 });
}

describe("createWarmCache", () => {
  it("drops and disposes an entry once when its scope falls below the tier", () => {
    const ws = make();
    const dispose = vi.fn();
    const cache = createWarmCache<string, number>(
      { kind: "tree", scopeOf, tier: "warm", dispose },
      ws,
    );
    for (const k of [A, B, C]) ws.activate(k);
    cache.set(`${A}/x`, 1);
    cache.set(`${B}/x`, 2);
    expect(cache.size()).toBe(2);
    ws.activate(D); // A suspended
    expect(cache.get(`${A}/x`)).toBeUndefined();
    expect(cache.get(`${B}/x`)).toBe(2);
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(dispose).toHaveBeenCalledWith(`${A}/x`, 1);
    expect(ws.debugCounts().caches.tree).toBe(1);
  });

  it("a suspended-tier cache survives suspension and drops on gone", () => {
    const ws = make();
    const cache = createWarmCache<string, number>(
      { kind: "scroll", scopeOf, tier: "suspended" },
      ws,
    );
    for (const k of [A, B, C]) ws.activate(k);
    cache.set(`${A}/x`, 1);
    ws.activate(D);
    expect(cache.get(`${A}/x`)).toBe(1);
    ws.activate(E);
    ws.activate(B);
    expect(cache.get(`${A}/x`)).toBeUndefined();
  });

  it("set replaces and disposes the previous value, and does not stack tier callbacks", () => {
    const ws = make();
    const dispose = vi.fn();
    const cache = createWarmCache<string, number>(
      { kind: "tree", scopeOf, tier: "warm", dispose },
      ws,
    );
    ws.activate(A);
    for (let i = 0; i < 30; i++) cache.set(`${A}/x`, i);
    expect(dispose).toHaveBeenCalledTimes(29);
    expect(cache.size()).toBe(1);
    cache.delete(`${A}/x`);
    expect(dispose).toHaveBeenCalledTimes(30);
    expect(ws.debugCounts().caches.tree ?? 0).toBe(0);
  });

  it("setting the same value again disposes nothing", () => {
    const ws = make();
    const dispose = vi.fn();
    const cache = createWarmCache<string, object>(
      { kind: "tree", scopeOf, tier: "warm", dispose },
      ws,
    );
    ws.activate(A);
    const v = {};
    cache.set(`${A}/x`, v);
    cache.set(`${A}/x`, v);
    expect(dispose).not.toHaveBeenCalled();
  });

  it("refuses an entry for a scope already below the tier, disposing it", () => {
    const ws = make();
    const dispose = vi.fn();
    const cache = createWarmCache<string, number>(
      { kind: "tree", scopeOf, tier: "warm", dispose },
      ws,
    );
    cache.set("unknown:scope/x", 1);
    expect(cache.size()).toBe(0);
    expect(dispose).toHaveBeenCalledWith("unknown:scope/x", 1);
  });

  it("update derives from the previous value", () => {
    const ws = make();
    const cache = createWarmCache<string, number>({ kind: "n", scopeOf, tier: "warm" }, ws);
    ws.activate(A);
    cache.update(`${A}/x`, (p) => (p ?? 0) + 1);
    cache.update(`${A}/x`, (p) => (p ?? 0) + 1);
    expect(cache.get(`${A}/x`)).toBe(2);
  });

  it("maxEntries evicts the oldest entry as a backstop", () => {
    const ws = make();
    const dispose = vi.fn();
    const cache = createWarmCache<string, number>(
      { kind: "n", scopeOf, tier: "warm", maxEntries: 2, dispose },
      ws,
    );
    ws.activate(A);
    cache.set(`${A}/1`, 1);
    cache.set(`${A}/2`, 2);
    cache.set(`${A}/3`, 3);
    expect(cache.get(`${A}/1`)).toBeUndefined();
    expect(dispose).toHaveBeenCalledWith(`${A}/1`, 1);
    expect(cache.size()).toBe(2);
  });

  it("forget drops the scope's entries; rekey moves them with rekeyKey, else drops", () => {
    const ws = make();
    const moved = createWarmCache<string, number>(
      {
        kind: "moved",
        scopeOf,
        tier: "warm",
        rekeyKey: (key, from, to) => key.replace(from, to),
      },
      ws,
    );
    const dropped = createWarmCache<string, number>({ kind: "dropped", scopeOf, tier: "warm" }, ws);
    ws.activate(A);
    moved.set(`${A}/x`, 1);
    dropped.set(`${A}/x`, 1);
    ws.rekey(A, "org1:a2");
    expect(moved.get("org1:a2/x")).toBe(1);
    expect(moved.get(`${A}/x`)).toBeUndefined();
    expect(dropped.size()).toBe(0);
    ws.forget("org1:a2");
    expect(moved.size()).toBe(0);
  });

  it("notifies subscribers of its own changes", () => {
    const ws = make();
    const cache = createWarmCache<string, number>({ kind: "n", scopeOf, tier: "warm" }, ws);
    const listener = vi.fn();
    cache.subscribe(listener);
    ws.activate(A);
    cache.set(`${A}/x`, 1);
    expect(listener).toHaveBeenCalledTimes(1);
  });
});
