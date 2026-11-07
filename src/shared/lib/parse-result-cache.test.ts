// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { createParseResultCache } from "./parse-result-cache";
import type { Block } from "./markdown-worker/types";

function mockBlocks(label: string): Block[] {
  return [{ id: 0, kind: "html", html: `<p>${label}</p>` }];
}

describe("parse-result-cache", () => {
  it("get miss returns undefined and increments misses", () => {
    const cache = createParseResultCache(3);
    expect(cache.get("/a.md", "content")).toBeUndefined();
    expect(cache.stats()).toEqual({ size: 0, hits: 0, misses: 1 });
  });

  it("set then get returns the stored blocks", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "hello", mockBlocks("A"));
    const b = cache.get("/a.md", "hello");
    expect(b).toBeDefined();
    expect(b![0].kind).toBe("html");
    if (b![0].kind !== "html") throw new Error("narrow");
    expect(b![0].html).toBe("<p>A</p>");
    expect(cache.stats()).toEqual({ size: 1, hits: 1, misses: 0 });
  });

  it("content change invalidates the cache (key is content-derived)", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "content v1", mockBlocks("V1"));
    expect(cache.get("/a.md", "content v1")).toBeDefined();
    expect(cache.get("/a.md", "content v2")).toBeUndefined();
  });

  it("same filePath with identical content hits regardless of call order", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "same", mockBlocks("A"));
    cache.set("/b.md", "same", mockBlocks("B"));
    // Same content but different path — keys differ.
    expect(
      cache.get("/a.md", "same")![0].kind === "html" &&
        (cache.get("/a.md", "same")![0] as { html: string }).html,
    ).toBe("<p>A</p>");
    expect(
      cache.get("/b.md", "same")![0].kind === "html" &&
        (cache.get("/b.md", "same")![0] as { html: string }).html,
    ).toBe("<p>B</p>");
  });

  it("evicts the least-recently-touched entry when set exceeds cap", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "A", mockBlocks("A"));
    cache.set("/b.md", "B", mockBlocks("B"));
    cache.set("/c.md", "C", mockBlocks("C"));

    // 'a' becomes MRU via a fresh get
    cache.get("/a.md", "A");

    // Adding 'd' overflows the cap — 'b' (now LRU) gets evicted.
    cache.set("/d.md", "D", mockBlocks("D"));

    expect(cache.get("/a.md", "A")).toBeDefined();
    expect(cache.get("/b.md", "B")).toBeUndefined();
    expect(cache.get("/c.md", "C")).toBeDefined();
    expect(cache.get("/d.md", "D")).toBeDefined();
  });

  it("set on an existing key overwrites in place and bumps freshness", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "unchanged", mockBlocks("v1"));
    cache.set("/a.md", "unchanged", mockBlocks("v2"));
    const got = cache.get("/a.md", "unchanged");
    if (got?.[0].kind !== "html") throw new Error("narrow");
    expect(got[0].html).toBe("<p>v2</p>");
  });

  it("invalidate drops every entry for a given filePath", () => {
    const cache = createParseResultCache(5);
    cache.set("/a.md", "v1", mockBlocks("A1"));
    cache.set("/a.md", "v2", mockBlocks("A2")); // different content
    cache.set("/b.md", "v1", mockBlocks("B"));
    cache.invalidate("/a.md");
    expect(cache.get("/a.md", "v1")).toBeUndefined();
    expect(cache.get("/a.md", "v2")).toBeUndefined();
    expect(cache.get("/b.md", "v1")).toBeDefined();
  });

  it("clear wipes entries and resets stats", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "A", mockBlocks("A"));
    cache.get("/a.md", "A");
    cache.get("/missing.md", "x");
    expect(cache.stats().size).toBe(1);
    expect(cache.stats().hits).toBe(1);
    expect(cache.stats().misses).toBe(1);
    cache.clear();
    expect(cache.stats()).toEqual({ size: 0, hits: 0, misses: 0 });
    expect(cache.get("/a.md", "A")).toBeUndefined();
  });

  it("cap of 1 always keeps only the most recent entry", () => {
    const cache = createParseResultCache(1);
    cache.set("/a.md", "A", mockBlocks("A"));
    cache.set("/b.md", "B", mockBlocks("B"));
    expect(cache.get("/a.md", "A")).toBeUndefined();
    expect(cache.get("/b.md", "B")).toBeDefined();
  });

  it("stats counts monotonically", () => {
    const cache = createParseResultCache(3);
    cache.set("/a.md", "A", mockBlocks("A"));
    cache.get("/a.md", "A");
    cache.get("/a.md", "A");
    cache.get("/missing.md", "x");
    expect(cache.stats()).toEqual({ size: 1, hits: 2, misses: 1 });
  });

  // Code-review patches below:

  it("filePath containing the old `|` delimiter doesn't corrupt invalidate() (NUL-delimited keys)", () => {
    // `|` is a legal filename character on POSIX/macOS. Before the
    // NUL delimiter, `invalidate("/a")` would drop entries keyed on
    // `/a|wierd.md` because both keys share the same `/a|` prefix.
    // NUL (`\x00`) is illegal in any filesystem path on every target
    // OS, so this collision is structurally prevented.
    const cache = createParseResultCache(5);
    cache.set("/a", "A-root", mockBlocks("A-root"));
    cache.set("/a|wierd.md", "A-weird", mockBlocks("A-weird"));
    cache.invalidate("/a");
    expect(cache.get("/a", "A-root")).toBeUndefined(); // dropped
    expect(cache.get("/a|wierd.md", "A-weird")).toBeDefined(); // preserved
  });

  it("cap of 0 short-circuits set() without allocating entries", () => {
    const cache = createParseResultCache(0);
    cache.set("/a.md", "A", mockBlocks("A"));
    cache.set("/b.md", "B", mockBlocks("B"));
    expect(cache.stats().size).toBe(0);
    expect(cache.get("/a.md", "A")).toBeUndefined();
    expect(cache.get("/b.md", "B")).toBeUndefined();
  });

  it("negative cap short-circuits set() (defensive against garbage input)", () => {
    const cache = createParseResultCache(-5);
    cache.set("/a.md", "A", mockBlocks("A"));
    expect(cache.stats().size).toBe(0);
  });
});
