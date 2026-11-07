// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { matchIndices, parseQuickOpenQuery, rankFiles } from "./editor-quick-open";

describe("parseQuickOpenQuery", () => {
  it("separates a file query from a line jump", () => {
    expect(parseQuickOpenQuery("button")).toEqual({ text: "button", line: null, column: null });
    expect(parseQuickOpenQuery(":42")).toEqual({ text: "", line: 42, column: null });
    expect(parseQuickOpenQuery(":42:7")).toEqual({ text: "", line: 42, column: 7 });
    expect(parseQuickOpenQuery("app.ts:10")).toEqual({ text: "app.ts", line: 10, column: null });
    // A bare ":" is go-to-line mode with no number yet (the palette checks the prefix).
    expect(parseQuickOpenQuery(":").line).toBeNull();
  });
});

describe("rankFiles", () => {
  const files = [
    "src/components/Button.tsx",
    "src/lib/button-utils.ts",
    "docs/buttons.md",
    "README.md",
  ];
  it("keeps only matches, file-name prefix matches first (shorter paths ahead)", () => {
    expect(rankFiles("butt", files)).toEqual([
      "docs/buttons.md",
      "src/lib/button-utils.ts",
      "src/components/Button.tsx",
    ]);
  });
  it("matches across path segments", () => {
    expect(rankFiles("cmpbtn", files)).toContain("src/components/Button.tsx");
  });
  it("lists files unfiltered for an empty query", () => {
    expect(rankFiles("", files)).toEqual(files);
  });
});

describe("matchIndices", () => {
  it("highlights a contiguous match, else a subsequence, else nothing", () => {
    expect(matchIndices("ton", "Button.tsx")).toEqual([3, 4, 5]);
    expect(matchIndices("btx", "Button.tsx")).toEqual([0, 2, 9]);
    expect(matchIndices("zzz", "Button.tsx")).toEqual([]);
  });
});

describe("rankFiles — ranking that matches what people type", () => {
  it("puts a file-name hit ahead of a folder-name hit", () => {
    expect(rankFiles("store", ["src/stores/index.ts", "src/lib/viewer-store.ts"])).toEqual([
      "src/lib/viewer-store.ts",
      "src/stores/index.ts",
    ]);
  });
  it("puts the exact file ahead of longer names that start the same", () => {
    const ranked = rankFiles("session", [
      "src/auth/session-store.test.ts",
      "src/auth/sessions.ts",
      "packages/a/src/auth/session.ts",
    ]);
    expect(ranked[0]).toBe("packages/a/src/auth/session.ts");
  });
  it("matches space-separated words anywhere in the path", () => {
    expect(rankFiles("auth sess", ["src/auth/session.ts", "src/lib/session.ts"])).toEqual([
      "src/auth/session.ts",
    ]);
  });
  it("ranks 20,000 paths fast enough to run on every keystroke", () => {
    const files = Array.from(
      { length: 20_000 },
      (_, i) => `packages/pkg-${i % 40}/src/${["lib", "hooks", "views"][i % 3]}/file-${i}.ts`,
    );
    const runs = [0, 1, 2].map(() => {
      const t0 = performance.now();
      rankFiles("view fil 19", files);
      return performance.now() - t0;
    });
    // A loose bound: the min of three runs, generous enough for a loaded CI box.
    expect(Math.min(...runs)).toBeLessThan(250);
  });
});
