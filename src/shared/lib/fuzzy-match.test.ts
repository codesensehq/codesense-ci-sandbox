// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { fuzzyIdTitleScore, fuzzyScore } from "./fuzzy-match";

describe("fuzzyScore", () => {
  it("empty query matches everything with score 1", () => {
    expect(fuzzyScore("", "anything")).toBe(1);
  });

  it("prefix match outranks substring match", () => {
    const prefix = fuzzyScore("foo", "foobar");
    const substring = fuzzyScore("foo", "xfoox");
    expect(prefix).toBeGreaterThan(substring);
  });

  it("substring match outranks fuzzy walk", () => {
    const substring = fuzzyScore("foo", "xfoox");
    const fuzzy = fuzzyScore("foo", "f-o-o");
    expect(substring).toBeGreaterThan(fuzzy);
  });

  it("shorter candidates rank higher among prefix matches", () => {
    const short = fuzzyScore("foo", "foo");
    const long = fuzzyScore("foo", "foobar-very-long");
    expect(short).toBeGreaterThan(long);
  });

  it("returns 0 when not all query chars are found", () => {
    expect(fuzzyScore("xyz", "abcdef")).toBe(0);
  });

  it("fuzzy walk credits word-boundary matches", () => {
    const withBoundary = fuzzyScore("abc", "ax-bx-cx");
    const withoutBoundary = fuzzyScore("abc", "axbxcx");
    expect(withBoundary).toBeGreaterThan(withoutBoundary);
  });

  it("is case-insensitive", () => {
    expect(fuzzyScore("FOO", "foobar")).toBe(fuzzyScore("foo", "foobar"));
  });

  describe("filenameFromPath mode", () => {
    it("prefix check runs against filename, not full path", () => {
      const withOpt = fuzzyScore("foo", "src/lib/foo.ts", { filenameFromPath: true });
      const withoutOpt = fuzzyScore("foo", "src/lib/foo.ts");
      // With the opt, "foo" prefix-matches the filename "foo.ts" → high score
      // Without, "foo" is a substring match only
      expect(withOpt).toBeGreaterThan(withoutOpt);
      expect(withOpt).toBeGreaterThan(100);
    });

    it("still works when candidate has no slash", () => {
      const withOpt = fuzzyScore("foo", "foobar", { filenameFromPath: true });
      const withoutOpt = fuzzyScore("foo", "foobar");
      expect(withOpt).toBe(withoutOpt);
    });
  });
});

describe("id and title ranking", () => {
  it("orders exact id, id prefix, title and other id matches", () => {
    const scores = [
      fuzzyIdTitleScore("sv-pbf-3", "SV-PBF-3", "Story"),
      fuzzyIdTitleScore("sv-pbf-3", "SV-PBF-30", "Story"),
      fuzzyIdTitleScore("sv-pbf-3", "OTHER", "Fix SV-PBF-3 picker"),
      fuzzyIdTitleScore("sv-pbf-3", "X-SV-PBF-3", "Story"),
    ];
    expect(scores.every((score, i) => i === 0 || score < scores[i - 1])).toBe(true);
  });
  it("shares case-insensitive fuzzy title matching and excludes nonmatches", () => {
    expect(fuzzyIdTitleScore("WPK", "SV-2", "Workspace picker")).toBeGreaterThan(0);
    expect(fuzzyIdTitleScore("WPK", "SV-2", "Workspace picker")).toBe(
      fuzzyIdTitleScore("wpk", "SV-2", "workspace picker"),
    );
    expect(fuzzyIdTitleScore("zzz", "SV-2", "Workspace picker")).toBe(0);
    expect(fuzzyIdTitleScore("", "SV-2", "Workspace picker")).toBe(1);
  });
  it("finds a query spanning the id and title below matches within either field", () => {
    const combined = fuzzyIdTitleScore("SV-2 Story", "SV-2", "Story workspace picker");
    expect(combined).toBeGreaterThan(0);
    expect(combined).toBeLessThan(fuzzyIdTitleScore("SV-2 Story", "OTHER", "SV-2 Story"));
  });
});
