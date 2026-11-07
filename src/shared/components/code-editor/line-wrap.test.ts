// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { resolveEditorLineWrap, DEFAULT_EDITOR_LINE_WRAP, type EditorLineWrap } from "./line-wrap";

describe("SW-VE-8 resolveEditorLineWrap", () => {
  it("defaults to auto", () => {
    expect(DEFAULT_EDITOR_LINE_WRAP).toBe("auto");
  });

  // Truth table: (setting, isCode) → effective wrap.
  const cases: Array<[EditorLineWrap, boolean, boolean]> = [
    // auto: wrap prose, not code
    ["auto", false, true], // prose → wrap
    ["auto", true, false], // code → no wrap
    // on: always wrap regardless of type
    ["on", false, true],
    ["on", true, true],
    // off: never wrap regardless of type
    ["off", false, false],
    ["off", true, false],
  ];
  it.each(cases)("setting=%s isCode=%s → %s", (setting, isCode, expected) => {
    expect(resolveEditorLineWrap(setting, isCode)).toBe(expected);
  });

  it("is TOTAL / fail-safe: a corrupted value resolves to the per-type default, never undefined", () => {
    // A present-but-invalid setting (the `??` guard only catches null/undefined).
    const bad = "garbage" as unknown as EditorLineWrap;
    expect(resolveEditorLineWrap(bad, false)).toBe(true); // prose → wrap (auto default)
    expect(resolveEditorLineWrap(bad, true)).toBe(false); // code → no wrap
    expect(resolveEditorLineWrap(bad, false)).not.toBeUndefined();
  });
});
