// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { pruneMapByKeys } from "./prune-map-by-keys";

describe("pruneMapByKeys", () => {
  it("returns the SAME instance when no key drops (referential stability)", () => {
    const map = new Map<string, boolean>([
      ["a", true],
      ["b", true],
    ]);
    const live = new Set(["a", "b", "c"]);
    const result = pruneMapByKeys(map, live);
    expect(result).toBe(map);
  });

  it("returns the SAME instance on the empty-map fast path", () => {
    const map = new Map<string, boolean>();
    const result = pruneMapByKeys(map, new Set(["a"]));
    expect(result).toBe(map);
  });

  it("returns a fresh pruned Map dropping keys not in liveKeys", () => {
    const map = new Map<string, boolean>([
      ["a", true],
      ["gone", true],
      ["b", true],
    ]);
    const live = new Set(["a", "b"]);
    const result = pruneMapByKeys(map, live);
    expect(result).not.toBe(map);
    expect([...result.keys()]).toEqual(["a", "b"]);
  });

  it("preserves the stored value for surviving keys", () => {
    const map = new Map<string, number>([
      ["a", 1],
      ["gone", 2],
      ["b", 3],
    ]);
    const result = pruneMapByKeys(map, new Set(["a", "b"]));
    expect(result.get("a")).toBe(1);
    expect(result.get("b")).toBe(3);
    expect(result.has("gone")).toBe(false);
  });

  it("returns an empty fresh Map when every key drops", () => {
    const map = new Map<string, boolean>([["a", true]]);
    const result = pruneMapByKeys(map, new Set<string>());
    expect(result).not.toBe(map);
    expect(result.size).toBe(0);
  });
});
