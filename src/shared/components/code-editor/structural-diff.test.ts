// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { diffArrays } from "diff";
import { tsxLanguage } from "@codemirror/lang-javascript";

import type { LanguageId } from "./cm-loader";
import {
  isStructuralLanguage,
  structuralOffsetChanges,
  treeToStructuralTokens,
  type StructuralToken,
} from "./structural-diff";
import { parseStructural } from "./structural-diff-parsers";

// A real parse → token projection, the way the worker builds it.
function tokensOf(src: string): StructuralToken[] {
  const tree = tsxLanguage.parser.parse(src);
  return treeToStructuralTokens(tree, (from, to) => src.slice(from, to));
}

describe("isStructuralLanguage", () => {
  it("is true for the Lezer-parse-tree languages", () => {
    for (const lang of ["typescript", "javascript", "json", "yaml", "python"] as const) {
      expect(isStructuralLanguage(lang)).toBe(true);
    }
  });

  it("is false for flat StreamLanguages, plain, and markdown (RedlineProse owns it)", () => {
    for (const lang of ["rust", "shell", "xml", "plain", "markdown"] as const) {
      expect(isStructuralLanguage(lang)).toBe(false);
    }
  });

  // The toggle gate (`isStructuralLanguage`) and the worker's parser switch
  // (`parseStructural`) are two enumerations of the same fact. If they drift, a
  // language reads as "supported" in the UI but the worker throws → the user sees
  // "failed" instead of "unsupported". `parseStructural`'s switch is exhaustive
  // over `LanguageId` (compile-checked), so this list only needs to cover it.
  it("agrees with parseStructural for every language (no set/parser drift)", () => {
    const allLanguages: LanguageId[] = [
      "typescript",
      "javascript",
      "json",
      "yaml",
      "python",
      "markdown",
      "shell",
      "rust",
      "xml",
      "plain",
    ];
    for (const lang of allLanguages) {
      expect(isStructuralLanguage(lang)).toBe(parseStructural("x", lang) !== null);
    }
  });
});

describe("treeToStructuralTokens", () => {
  it("drops whitespace/indentation so a reindent yields an identical token stream", () => {
    const a = tokensOf("function f(){return 1}");
    const b = tokensOf("function f() {\n    return 1\n}\n");
    expect(a.map((t) => t.sig)).toEqual(b.map((t) => t.sig));
  });

  it("records real offsets for each token", () => {
    const src = "const x = 1";
    const tokens = tokensOf(src);
    for (const t of tokens) {
      expect(src.slice(t.from, t.to)).toBe(t.sig);
      expect(t.to).toBeGreaterThan(t.from);
    }
    expect(tokens.map((t) => t.sig)).toContain("const");
    expect(tokens.map((t) => t.sig)).toContain("x");
  });

  it("differs when the code actually changes", () => {
    const a = tokensOf("const x = 1").map((t) => t.sig);
    const b = tokensOf("const y = 1").map((t) => t.sig);
    expect(a).not.toEqual(b);
  });
});

describe("structuralOffsetChanges", () => {
  const run = (a: StructuralToken[], b: StructuralToken[]) =>
    structuralOffsetChanges(a, b, (x, y) => diffArrays(x, y));

  it("returns no changes for a formatting-only reflow (parse-tree language)", () => {
    const before = "function f(){return 1}";
    const after = "function f() {\n  return 1\n}\n";
    expect(run(tokensOf(before), tokensOf(after))).toEqual([]);
  });

  it("returns no changes when the streams are identical", () => {
    expect(run(tokensOf("const a = 1"), tokensOf("const a = 1"))).toEqual([]);
  });

  it("spans a real edit in original coordinates on both sides", () => {
    const before = "const x = 1";
    const after = "const y = 1";
    const changes = run(tokensOf(before), tokensOf(after));
    expect(changes).toHaveLength(1);
    const c = changes[0];
    expect(before.slice(c.fromA, c.toA)).toBe("x");
    expect(after.slice(c.fromB, c.toB)).toBe("y");
  });

  it("models a pure insertion as an empty A-span", () => {
    const before = "a\n";
    const after = "a\nb\n";
    const changes = run(tokensOf(before), tokensOf(after));
    expect(changes).toHaveLength(1);
    const c = changes[0];
    expect(c.fromA).toBe(c.toA); // nothing removed from A
    expect(after.slice(c.fromB, c.toB)).toContain("b");
  });

  it("models a pure deletion as an empty B-span", () => {
    const before = "a\nb\n";
    const after = "a\n";
    const changes = run(tokensOf(before), tokensOf(after));
    expect(changes).toHaveLength(1);
    const c = changes[0];
    expect(c.fromB).toBe(c.toB); // nothing added to B
    expect(before.slice(c.fromA, c.toA)).toContain("b");
  });

  it("emits ordered, non-overlapping regions for multiple edits", () => {
    const before = "const a = 1\nconst b = 2\nconst c = 3";
    const after = "const a = 9\nconst b = 2\nconst c = 8";
    const changes = run(tokensOf(before), tokensOf(after));
    expect(changes.length).toBeGreaterThanOrEqual(2);
    for (let i = 1; i < changes.length; i++) {
      expect(changes[i].fromA).toBeGreaterThanOrEqual(changes[i - 1].toA);
      expect(changes[i].fromB).toBeGreaterThanOrEqual(changes[i - 1].toB);
    }
  });
});
