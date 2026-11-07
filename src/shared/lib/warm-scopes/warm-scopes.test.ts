// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { WarmScopes } from "./warm-scopes";

const A = "org1:a";
const B = "org1:b";
const C = "org2:c";
const D = "org1:d";
const E = "org2:e";

function make(config?: { warmCapacity?: number; suspendedAfterSwitches?: number }) {
  return new WarmScopes({ warmCapacity: 3, suspendedAfterSwitches: 4, ...config });
}

describe("tiers and LRU", () => {
  it("the activated scope is active and the one it replaced is warm", () => {
    const ws = make();
    ws.activate(A);
    ws.activate(B);
    expect(ws.tierOf(B)).toBe("active");
    expect(ws.tierOf(A)).toBe("warm");
    expect(ws.tierOf("never:seen")).toBe("gone");
  });

  it("keeps warm capacity 3 including the active scope, spanning orgs", () => {
    const ws = make();
    for (const k of [A, C, B]) ws.activate(k);
    expect([ws.tierOf(A), ws.tierOf(C), ws.tierOf(B)]).toEqual(["warm", "warm", "active"]);
    ws.activate(D);
    expect(ws.tierOf(A)).toBe("suspended");
    expect(ws.tierOf(C)).toBe("warm");
    expect(ws.tierOf(B)).toBe("warm");
    expect(ws.tierOf(D)).toBe("active");
  });

  it("re-activating a warm scope moves it to the front without suspending anything", () => {
    const ws = make();
    for (const k of [A, B, C]) ws.activate(k);
    ws.activate(A);
    expect([ws.tierOf(A), ws.tierOf(B), ws.tierOf(C)]).toEqual(["active", "warm", "warm"]);
  });

  it("re-activating the active scope is a no-op", () => {
    const ws = make();
    ws.activate(A);
    const listener = vi.fn();
    ws.subscribe(listener);
    ws.activate(A);
    expect(listener).not.toHaveBeenCalled();
    expect(ws.debugCounts().activations).toBe(1);
  });

  it("activate(null) leaves the last active scope warm", () => {
    const ws = make();
    ws.activate(A);
    ws.activate(null);
    expect(ws.tierOf(A)).toBe("warm");
  });

  it("suspended becomes gone after more than the threshold of switches", () => {
    const ws = make();
    const gone = vi.fn();
    ws.setGoneHandler(gone);
    ws.activate(A); // activations 1
    for (const k of [B, C, D]) ws.activate(k); // A suspended at 4
    expect(ws.tierOf(A)).toBe("suspended");
    ws.activate(E); // 5: 5 - 1 = 4, not yet over
    expect(ws.tierOf(A)).toBe("suspended");
    expect(gone).not.toHaveBeenCalled();
    ws.activate(B); // 6: 6 - 1 = 5 > 4
    expect(ws.tierOf(A)).toBe("gone");
    expect(gone).toHaveBeenCalledTimes(1);
    expect(gone).toHaveBeenCalledWith(A);
  });

  it("a re-activated suspended scope is active again and not evicted", () => {
    const ws = make();
    const gone = vi.fn();
    ws.setGoneHandler(gone);
    for (const k of [A, B, C, D]) ws.activate(k);
    expect(ws.tierOf(A)).toBe("suspended");
    ws.activate(A);
    expect(ws.tierOf(A)).toBe("active");
    for (const k of [B, C, D]) ws.activate(k);
    expect(gone).not.toHaveBeenCalled();
  });

  it("orgTier is derived from the org's teams", () => {
    const ws = make();
    ws.activate(A);
    ws.activate(C);
    expect(ws.orgTier("org2")).toBe("active");
    expect(ws.orgTier("org1")).toBe("warm");
    expect(ws.orgTier("org9")).toBe("cold");
  });

  it("notifies subscribers once per change and exposes a changing version", () => {
    const ws = make();
    const listener = vi.fn();
    ws.subscribe(listener);
    const v0 = ws.getVersion();
    ws.activate(A);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(ws.getVersion()).not.toBe(v0);
  });
});

describe("onTier", () => {
  it("fires once when the scope drops below the floor", () => {
    const ws = make();
    const cb = vi.fn();
    for (const k of [A, B]) ws.activate(k);
    ws.onTier(A, "warm", cb);
    ws.activate(C);
    expect(cb).not.toHaveBeenCalled();
    ws.activate(D); // A: warm -> suspended
    expect(cb).toHaveBeenCalledTimes(1);
    ws.activate(E);
    ws.activate(A);
    ws.activate(E);
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("a suspended floor fires only on gone", () => {
    const ws = make();
    const cb = vi.fn();
    for (const k of [A, B, C, D]) ws.activate(k);
    ws.onTier(A, "suspended", cb);
    expect(cb).not.toHaveBeenCalled();
    ws.activate(E);
    ws.activate(B);
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("a released callback never fires", () => {
    const ws = make();
    const cb = vi.fn();
    for (const k of [A, B, C]) ws.activate(k);
    const off = ws.onTier(A, "warm", cb);
    off();
    ws.activate(D);
    expect(cb).not.toHaveBeenCalled();
  });
});

describe("hold", () => {
  it("disposes exactly once on release, even if released twice", () => {
    const ws = make();
    ws.activate(A);
    const dispose = vi.fn();
    const release = ws.hold(A, "watch", "k", dispose);
    expect(ws.debugCounts().held[A]).toEqual({ watch: 1 });
    release();
    release();
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(ws.debugCounts().holdsTotal).toBe(0);
    expect(ws.debugCounts().disposed.watch).toBe(1);
  });

  it("replaces on re-acquire: the old entry is disposed and the count stays flat", () => {
    const ws = make();
    ws.activate(A);
    const first = vi.fn();
    const second = vi.fn();
    const releaseFirst = ws.hold(A, "watch", "k", first);
    for (let i = 0; i < 20; i++) ws.hold(A, "watch", "k", vi.fn());
    ws.hold(A, "watch", "k", second);
    expect(first).toHaveBeenCalledTimes(1);
    expect(ws.debugCounts().held[A]).toEqual({ watch: 1 });
    // A stale release must not dispose the newer entry.
    releaseFirst();
    expect(second).not.toHaveBeenCalled();
    expect(ws.debugCounts().holdsTotal).toBe(1);
  });

  it("runs the bag when the scope goes gone, once", () => {
    const ws = make();
    const dispose = vi.fn();
    for (const k of [A, B, C, D]) ws.activate(k);
    ws.hold(A, "frame", "f", dispose);
    ws.activate(E);
    expect(dispose).not.toHaveBeenCalled();
    ws.activate(B);
    expect(ws.tierOf(A)).toBe("gone");
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("a warm floor disposes when the scope is suspended", () => {
    const ws = make();
    const dispose = vi.fn();
    for (const k of [A, B, C]) ws.activate(k);
    ws.hold(A, "frame", "f", dispose, "warm");
    ws.activate(D);
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("a throwing disposer does not skip the rest", () => {
    const ws = make();
    ws.activate(A);
    const after = vi.fn();
    ws.hold(A, "x", "1", () => {
      throw new Error("boom");
    });
    ws.hold(A, "x", "2", after);
    ws.forget(A);
    expect(after).toHaveBeenCalledTimes(1);
    expect(ws.debugCounts().holdsTotal).toBe(0);
  });
});

describe("forget, rekey, pin", () => {
  it("forget disposes everything for the scope and its tier becomes gone", () => {
    const ws = make();
    const d1 = vi.fn();
    const d2 = vi.fn();
    const cb = vi.fn();
    ws.activate(A);
    ws.activate(B);
    ws.hold(A, "k", "1", d1);
    ws.hold(A, "k", "2", d2);
    ws.onTier(A, "warm", cb);
    ws.forget(A);
    expect(d1).toHaveBeenCalledTimes(1);
    expect(d2).toHaveBeenCalledTimes(1);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(ws.tierOf(A)).toBe("gone");
    expect(ws.tierOf(B)).toBe("active");
  });

  it("forgetting the active scope leaves no active scope", () => {
    const ws = make();
    ws.activate(A);
    ws.forget(A);
    expect(ws.tierOf(A)).toBe("gone");
    ws.activate(B);
    expect(ws.tierOf(B)).toBe("active");
  });

  it("forget does not call the gone handler (the caller already closed the team)", () => {
    const ws = make();
    const gone = vi.fn();
    ws.setGoneHandler(gone);
    ws.activate(A);
    ws.forget(A);
    expect(gone).not.toHaveBeenCalled();
  });

  it("rekey moves the tier, holds and callbacks without disposing", () => {
    const ws = make();
    const dispose = vi.fn();
    const cb = vi.fn();
    ws.activate(A);
    ws.activate(B);
    ws.hold(A, "k", "1", dispose);
    ws.onTier(A, "warm", cb);
    ws.rekey(A, "org1:a2");
    expect(ws.tierOf(A)).toBe("gone");
    expect(ws.tierOf("org1:a2")).toBe("warm");
    expect(dispose).not.toHaveBeenCalled();
    expect(ws.debugCounts().held["org1:a2"]).toEqual({ k: 1 });
    expect(ws.debugCounts().held[A]).toBeUndefined();
    ws.activate(C);
    ws.activate(D); // a2: warm -> suspended: its moved callback fires
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("rekey of the active scope keeps it active", () => {
    const ws = make();
    ws.activate(A);
    ws.rekey(A, "org1:a2");
    expect(ws.tierOf("org1:a2")).toBe("active");
  });

  it("a pinned scope never drops below warm and is not evicted until released", () => {
    const ws = make();
    const gone = vi.fn();
    ws.setGoneHandler(gone);
    ws.activate(A);
    const unpin = ws.pin(A, "dirty");
    for (const k of [B, C, D, E, B, C, D]) ws.activate(k);
    expect(ws.tierOf(A)).toBe("warm");
    expect(gone).not.toHaveBeenCalled();
    unpin();
    unpin();
    expect(ws.tierOf(A)).toBe("gone");
    expect(gone).toHaveBeenCalledWith(A);
  });

  it("a pin released after a rename unpins the moved scope, which then ages out", () => {
    const ws = make();
    const gone = vi.fn();
    ws.setGoneHandler(gone);
    ws.activate(A);
    const unpin = ws.pin(A, "dirty");
    ws.rekey(A, "org1:a2");
    for (const k of [B, C, D, E]) ws.activate(k);
    expect(ws.tierOf("org1:a2")).toBe("warm");
    unpin();
    for (const k of [B, C, D]) ws.activate(k);
    expect(ws.tierOf("org1:a2")).toBe("gone");
    expect(gone).toHaveBeenCalledWith("org1:a2");
  });

  it("a pin keeps the scope's holds alive", () => {
    const ws = make();
    const dispose = vi.fn();
    ws.activate(A);
    ws.hold(A, "k", "1", dispose, "warm");
    ws.pin(A, "dirty");
    for (const k of [B, C, D]) ws.activate(k);
    expect(dispose).not.toHaveBeenCalled();
  });
});

describe("debugCounts", () => {
  it("reports tiers and activations", () => {
    const ws = make();
    ws.activate(A);
    ws.activate(B);
    const c = ws.debugCounts();
    expect(c.tiers).toEqual({ [A]: "warm", [B]: "active" });
    expect(c.activations).toBe(2);
  });
});
