// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { Change, diff } from "@codemirror/merge";

import {
  createIgnoreWhitespaceOverride,
  normalizeIgnoringWhitespace,
  remapChange,
  type OffsetChange,
} from "./whitespace-diff";

describe("normalizeIgnoringWhitespace", () => {
  it("drops intra-line whitespace but keeps newlines", () => {
    const { text } = normalizeIgnoringWhitespace("  a  b\n\tc \n");
    expect(text).toBe("ab\nc\n");
  });

  it("treats carriage returns as droppable (CRLF === LF)", () => {
    expect(normalizeIgnoringWhitespace("a\r\nb").text).toBe("a\nb");
  });

  it("maps every retained char back to its original index, plus an end sentinel", () => {
    const { text, map } = normalizeIgnoringWhitespace("  ab");
    expect(text).toBe("ab");
    // 'a' is at original index 2, 'b' at 3, sentinel = original length 4.
    expect(map).toEqual([2, 3, 4]);
  });

  it("map length is always text.length + 1 so an end-exclusive offset is valid", () => {
    const img = normalizeIgnoringWhitespace("x = 1\n  y = 2\n");
    expect(img.map).toHaveLength(img.text.length + 1);
    expect(img.map[img.text.length]).toBe("x = 1\n  y = 2\n".length);
  });
});

describe("remapChange", () => {
  it("translates normalized offsets through the maps", () => {
    const a = normalizeIgnoringWhitespace("  foo");
    const b = normalizeIgnoringWhitespace("bar");
    const remapped = remapChange({ fromA: 0, toA: 3, fromB: 0, toB: 3 }, a.map, b.map);
    // fromA 0 → original index 2 (after the two leading spaces); toA 3 → end (5).
    expect(remapped).toEqual({ fromA: 2, toA: 5, fromB: 0, toB: 3 });
  });
});

describe("createIgnoreWhitespaceOverride (seam: real @codemirror/merge diff)", () => {
  const override = createIgnoreWhitespaceOverride<Change>(
    diff,
    (fromA, toA, fromB, toB) => new Change(fromA, toA, fromB, toB),
    { scanLimit: 500, timeout: 1000 },
  );

  it("reports NO changes for a whitespace-only reindent", () => {
    const a = "function f() {\nreturn 1;\n}\n";
    const b = "function f() {\n    return 1;\n}\n";
    expect(override(a, b)).toHaveLength(0);
  });

  it("reports NO changes for trailing-whitespace-only differences", () => {
    expect(override("a = 1\nb = 2\n", "a = 1   \nb = 2\t\n")).toHaveLength(0);
  });

  it("still reports a REAL edit, remapped to original coordinates", () => {
    const a = "const x = 1;\n";
    const b = "const x = 2;\n"; // 1 → 2, real change
    const changes = override(a, b);
    expect(changes.length).toBeGreaterThan(0);
    // The change must land on the digit in the ORIGINAL doc, not a normalized offset.
    const c = changes[0];
    expect(a.slice(c.fromA, c.toA)).toContain("1");
    expect(b.slice(c.fromB, c.toB)).toContain("2");
  });

  it("a real edit buried in reindented code is found and whitespace churn is ignored", () => {
    const a = "if (ok) {\ndo(1);\n}\n";
    const b = "if (ok) {\n        do(2);\n}\n"; // reindented AND 1→2
    const changes = override(a, b);
    expect(changes.length).toBeGreaterThan(0);
    // Every reported change is a genuine content change (non-whitespace on some side).
    for (const c of changes) {
      const aSlice = a.slice(c.fromA, c.toA);
      const bSlice = b.slice(c.fromB, c.toB);
      expect((aSlice + bSlice).trim().length).toBeGreaterThan(0);
    }
  });

  it("returns an empty diff for identical input", () => {
    const s = "x\ny\nz\n";
    expect(override(s, s)).toHaveLength(0);
  });

  it("normalized-offset changes from the injected diff are remapped (unit, stubbed diff)", () => {
    const stub = (): readonly OffsetChange[] => [{ fromA: 0, toA: 1, fromB: 0, toB: 1 }];
    const ov = createIgnoreWhitespaceOverride<OffsetChange>(stub, (fromA, toA, fromB, toB) => ({
      fromA,
      toA,
      fromB,
      toB,
    }));
    // Leading whitespace pushes original coords: "  X" → normalized "X" at orig 2.
    expect(ov("  X", "  Y")).toEqual([{ fromA: 2, toA: 3, fromB: 2, toB: 3 }]);
  });
});
