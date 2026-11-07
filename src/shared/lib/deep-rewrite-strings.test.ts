// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { deepRewriteStrings } from "./deep-rewrite-strings";

const upper = (s: string) => s.toUpperCase();

describe("deepRewriteStrings", () => {
  it("rewrites strings at every depth across objects and arrays", () => {
    const node = {
      a: "one",
      nested: { b: "two", list: ["three", { c: "four" }] },
    };
    deepRewriteStrings(node, upper);
    expect(node).toEqual({
      a: "ONE",
      nested: { b: "TWO", list: ["THREE", { c: "FOUR" }] },
    });
  });

  it("leaves non-string leaves (number, boolean, null) untouched", () => {
    const node: Record<string, unknown> = { n: 1, b: true, z: null, s: "x" };
    deepRewriteStrings(node, upper);
    expect(node).toEqual({ n: 1, b: true, z: null, s: "X" });
  });

  it("only reassigns when the rewrite actually changes the value (idempotent)", () => {
    const node = { s: "already" };
    // A no-op rewrite must leave the value referentially stable and unchanged.
    deepRewriteStrings(node, (s) => s);
    expect(node.s).toBe("already");
    // A second pass of an idempotent transform changes nothing further.
    deepRewriteStrings(node, upper);
    const after = node.s;
    deepRewriteStrings(node, upper);
    expect(node.s).toBe(after);
  });

  it("tolerates a top-level array and a null/primitive root", () => {
    const arr = ["a", ["b"]];
    deepRewriteStrings(arr, upper);
    expect(arr).toEqual(["A", ["B"]]);
    // Non-container roots are a safe no-op.
    expect(() => deepRewriteStrings(null, upper)).not.toThrow();
    expect(() => deepRewriteStrings("bare", upper)).not.toThrow();
  });
});
