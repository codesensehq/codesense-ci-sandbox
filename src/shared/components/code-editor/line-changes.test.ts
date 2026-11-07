// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import { computeLineChanges, resetLineChangesCache, summarizeLineChanges } from "./line-changes";

const lines = (...xs: string[]) => xs.join("\n");

describe("computeLineChanges", () => {
  it("reports nothing for identical text", () => {
    expect(computeLineChanges("a\nb", "a\nb")).toEqual([]);
  });

  it("marks an edited line modified", () => {
    expect(computeLineChanges(lines("a", "b", "c"), lines("a", "B", "c"))).toEqual([
      { line: 2, kind: "modified" },
    ]);
  });

  it("marks inserted lines added", () => {
    expect(computeLineChanges(lines("a", "c"), lines("a", "b1", "b2", "c"))).toEqual([
      { line: 2, kind: "added" },
      { line: 3, kind: "added" },
    ]);
  });

  it("anchors a pure deletion on the line below it", () => {
    expect(computeLineChanges(lines("a", "b", "c"), lines("a", "c"))).toEqual([
      { line: 2, kind: "deleted" },
    ]);
  });

  it("clamps a deletion at the end of the file to the last line", () => {
    expect(computeLineChanges(lines("a", "b", "c"), lines("a", "b"))).toEqual([
      { line: 2, kind: "deleted" },
    ]);
  });

  it("pairs a replaced block as modified and the overflow as added", () => {
    expect(computeLineChanges(lines("a", "x", "z"), lines("a", "X1", "X2", "z"))).toEqual([
      { line: 2, kind: "modified" },
      { line: 3, kind: "added" },
    ]);
  });

  it("finds separate hunks across unchanged lines", () => {
    const base = lines("1", "2", "3", "4", "5", "6");
    const cur = lines("1", "two", "3", "4", "5", "6", "7");
    expect(computeLineChanges(base, cur)).toEqual([
      { line: 2, kind: "modified" },
      { line: 7, kind: "added" },
    ]);
  });

  it("treats every line of a brand-new file as added", () => {
    expect(computeLineChanges("", lines("a", "b"))).toEqual([
      { line: 1, kind: "added" },
      { line: 2, kind: "added" },
    ]);
  });

  it("counts a new file's lines without a phantom line after its final newline", () => {
    const changes = computeLineChanges("", "a\nb\n");
    expect(changes).toEqual([
      { line: 1, kind: "added" },
      { line: 2, kind: "added" },
    ]);
    expect(summarizeLineChanges(changes)).toEqual({ added: 2, modified: 0, deleted: 0 });
    expect(computeLineChanges("", "a\r\nb\r\n")).toHaveLength(2);
  });

  it("ignores a CRLF vs LF difference", () => {
    expect(computeLineChanges("a\r\nb\r\n", "a\nb\n")).toEqual([]);
  });

  it("stays bounded on a wholesale rewrite", () => {
    const base = Array.from({ length: 6000 }, (_, i) => `old ${i}`).join("\n");
    const cur = Array.from({ length: 6000 }, (_, i) => `new ${i}`).join("\n");
    const started = performance.now();
    const changes = computeLineChanges(base, cur);
    expect(performance.now() - started).toBeLessThan(2000);
    expect(summarizeLineChanges(changes)).toEqual({ added: 0, modified: 6000, deleted: 0 });
  });

  it("a rewrite re-diffed every keystroke doesn't rerun the capped search", () => {
    resetLineChangesCache();
    const base = Array.from({ length: 6000 }, (_, i) => `old ${i}`).join("\n");
    const cur = (tag: string) =>
      Array.from({ length: 6000 }, (_, i) => `new ${i}${i === 3000 ? tag : ""}`).join("\n");
    const t0 = performance.now();
    const first = computeLineChanges(base, cur("a"));
    const searched = performance.now() - t0;
    const t1 = performance.now();
    const again = computeLineChanges(base, cur("ab"));
    const cached = performance.now() - t1;
    expect(again).toEqual(first);
    // The remembered give-up skips the search: well under the first pass.
    expect(cached).toBeLessThan(Math.max(searched / 2, 50));
  });

  it("a remembered give-up never hides a same-shape buffer that became diffable", () => {
    resetLineChangesCache();
    const body = Array.from({ length: 1200 }, (_, i) => `line ${i}`);
    const base = body.join("\n");
    // A formatter wraps the whole file in a block and indents everything...
    const wrapped = ["wrap {", ...body.map((l) => `  ${l}`), "}"].join("\n");
    expect(summarizeLineChanges(computeLineChanges(base, wrapped)).modified).toBe(1200);
    // ...then the body is outdented, keeping the wrapper: same middle size, two edits.
    const outdented = ["wrap {", ...body, "}"].join("\n");
    expect(computeLineChanges(base, outdented)).toEqual([
      { line: 1, kind: "added" },
      { line: 1202, kind: "added" },
    ]);
  });

  it("a size gap past the cap reports the middle without searching", () => {
    const base = lines("a", "z");
    const cur = ["a", ...Array.from({ length: 1500 }, (_, i) => `n${i}`), "z"].join("\n");
    expect(summarizeLineChanges(computeLineChanges(base, cur))).toEqual({
      added: 1500,
      modified: 0,
      deleted: 0,
    });
  });
});
