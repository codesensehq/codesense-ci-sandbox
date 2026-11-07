// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { createFileContentCache, fileContentCache } from "./file-content-cache";
import { VIEWER_PERF } from "./viewer-perf-config";

describe("file-content-cache (factory)", () => {
  it("get miss returns undefined", () => {
    const cache = createFileContentCache(3);
    expect(cache.get("/missing.md")).toBeUndefined();
  });

  it("set then get returns the stored content + refreshKey", () => {
    const cache = createFileContentCache(3);
    cache.set("/a.md", "A", 1);
    const entry = cache.get("/a.md");
    expect(entry?.content).toBe("A");
    expect(entry?.refreshKey).toBe(1);
  });

  it("set on an existing key overwrites content and bumps freshness", () => {
    const cache = createFileContentCache(3);
    cache.set("/a.md", "A-old", 0);
    cache.set("/a.md", "A-new", 1);
    const entry = cache.get("/a.md");
    expect(entry?.content).toBe("A-new");
    expect(entry?.refreshKey).toBe(1);
  });

  it("updates freshness while preserving identical content", () => {
    const cache = createFileContentCache(3);
    cache.set("/a.md", "same", 0);
    cache.set("/a.md", "same", 1);
    expect(cache.get("/a.md")).toMatchObject({ content: "same", refreshKey: 1 });
  });

  it("evicts the least-recently-touched entry when set exceeds cap", () => {
    const cache = createFileContentCache(3);
    cache.set("/a.md", "A", 0);
    cache.set("/b.md", "B", 0);
    cache.set("/c.md", "C", 0);

    // 'a' becomes MRU
    cache.get("/a.md");

    // Adding 'd' overflows the cap — 'b' (now LRU) gets evicted, not 'a'.
    cache.set("/d.md", "D", 0);

    expect(cache.get("/a.md")?.content).toBe("A");
    expect(cache.get("/b.md")).toBeUndefined();
    expect(cache.get("/c.md")?.content).toBe("C");
    expect(cache.get("/d.md")?.content).toBe("D");
  });

  it("invalidate removes a single entry", () => {
    const cache = createFileContentCache(3);
    cache.set("/a.md", "A", 0);
    cache.set("/b.md", "B", 0);
    cache.invalidate("/a.md");

    expect(cache.get("/a.md")).toBeUndefined();
    expect(cache.get("/b.md")?.content).toBe("B");
  });

  it("clear wipes every entry", () => {
    const cache = createFileContentCache(3);
    cache.set("/a.md", "A", 0);
    cache.set("/b.md", "B", 0);
    cache.clear();

    expect(cache.get("/a.md")).toBeUndefined();
    expect(cache.get("/b.md")).toBeUndefined();
  });

  it("cap of 1 always keeps only the most recent entry", () => {
    const cache = createFileContentCache(1);
    cache.set("/a.md", "A", 0);
    cache.set("/b.md", "B", 0);
    expect(cache.get("/a.md")).toBeUndefined();
    expect(cache.get("/b.md")?.content).toBe("B");
  });
});

describe("file-content-cache (default instance)", () => {
  it("default instance's cap matches VIEWER_PERF.fileCacheMaxEntries", () => {
    fileContentCache.clear();
    // Fill exactly up to the cap; nothing should evict.
    for (let i = 0; i < VIEWER_PERF.fileCacheMaxEntries; i++) {
      fileContentCache.set(`/file-${i}.md`, String(i), 0);
    }
    for (let i = 0; i < VIEWER_PERF.fileCacheMaxEntries; i++) {
      expect(fileContentCache.get(`/file-${i}.md`)).toBeDefined();
    }

    // One more overflows — the first entry (LRU) should be evicted.
    // (Getting every entry above also touches them in order, so the
    // LRU is `/file-0.md` — the earliest set.)
    // Re-seed to guarantee file-0 is LRU: set file-0 first, then get
    // every other entry to bump their touchedAt, then overflow.
    fileContentCache.clear();
    fileContentCache.set("/file-0.md", "0", 0);
    for (let i = 1; i < VIEWER_PERF.fileCacheMaxEntries; i++) {
      fileContentCache.set(`/file-${i}.md`, String(i), 0);
    }
    // Now the cache is full; file-0 was set earliest.
    fileContentCache.set("/overflow.md", "X", 0);
    expect(fileContentCache.get("/file-0.md")).toBeUndefined();
    expect(fileContentCache.get("/overflow.md")?.content).toBe("X");

    fileContentCache.clear();
  });
});
