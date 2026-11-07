// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { findSubtreeByScope } from "./find-subtree-by-scope";
import { makeEpic, makeStory } from "../doc-query/__fixtures__/factory";

function tree() {
  const a = makeEpic("/ctx/epic-a", "epic-a", [makeStory("/ctx/epic-a", "a1.md")]);
  const bInner = makeEpic("/ctx/epic-b/inner", "inner", [makeStory("/ctx/epic-b/inner", "i1.md")]);
  const b = makeEpic("/ctx/epic-b", "epic-b", [], [bInner]);
  return makeEpic("/ctx", "", [], [a, b]);
}

describe("findSubtreeByScope", () => {
  it("resolves a sub-epic by dirName", () => {
    expect(findSubtreeByScope(tree(), "epic-a")?.absoluteDirPath).toBe("/ctx/epic-a");
  });

  it("resolves a nested sub-epic by name", () => {
    expect(findSubtreeByScope(tree(), "inner")?.absoluteDirPath).toBe("/ctx/epic-b/inner");
  });

  it("resolves by absolute path", () => {
    expect(findSubtreeByScope(tree(), "/ctx/epic-b")?.dirName).toBe("epic-b");
  });

  it("returns null for an unknown scope (no throw)", () => {
    expect(findSubtreeByScope(tree(), "does-not-exist")).toBeNull();
  });

  it("returns null for an empty scope", () => {
    expect(findSubtreeByScope(tree(), "   ")).toBeNull();
  });

  it("does not false-match on a bare suffix substring", () => {
    // "a" must not match "/ctx/epic-a" (separator-guarded).
    expect(findSubtreeByScope(tree(), "a")).toBeNull();
  });
});
