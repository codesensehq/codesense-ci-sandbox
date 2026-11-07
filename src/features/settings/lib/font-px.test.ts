// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { commitFontPxOverride } from "./font-px";

describe("commitFontPxOverride", () => {
  it("returns null for non-numeric input", () => {
    expect(commitFontPxOverride("", 14)).toBeNull();
    expect(commitFontPxOverride("abc", 14)).toBeNull();
    expect(commitFontPxOverride("--", 14)).toBeNull();
  });

  it("rejects decimals, negatives, and signed values", () => {
    expect(commitFontPxOverride("14.5", 14)).toBeNull();
    expect(commitFontPxOverride("-5", 14)).toBeNull();
    expect(commitFontPxOverride("+14", 14)).toBeNull();
    expect(commitFontPxOverride("0", 14)).toBeNull();
  });

  it("accepts values padded with whitespace", () => {
    expect(commitFontPxOverride("  16  ", 14)).toBe(16);
    expect(commitFontPxOverride("\t14\n", 14)).toBe(0);
  });

  it("returns 0 (clear override) when clamped value equals canonical preset px", () => {
    expect(commitFontPxOverride("14", 14)).toBe(0);
  });

  it("returns the clamped px when it differs from the canonical preset", () => {
    expect(commitFontPxOverride("18", 14)).toBe(18);
  });

  it("clamps below FONT_PX_MIN to FONT_PX_MIN (8)", () => {
    expect(commitFontPxOverride("4", 14)).toBe(8);
  });

  it("clamps above FONT_PX_MAX to FONT_PX_MAX (40)", () => {
    expect(commitFontPxOverride("100", 14)).toBe(40);
  });

  it("clears override when an out-of-range typed value clamps to the canonical preset", () => {
    // typed=4 → clamps to 8 → if preset canonical is 8, clear.
    expect(commitFontPxOverride("4", 8)).toBe(0);
  });
});
