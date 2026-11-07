// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { relativeLuminance, contrastRatio, compositeOver, mixHex } from "./color-contrast";

it("composites alpha before measuring contrast", () => {
  expect(compositeOver("#ffffff80", "#000000")).toBe("#808080");
  expect(compositeOver("#fff0", "#123456")).toBe("#123456");
  expect(mixHex("#000000", "#ffffff", 0.5)).toBe("#808080");
  expect(contrastRatio("#000000", "#ffffff")).toBeCloseTo(21);
});

describe("relativeLuminance", () => {
  it("is 1 for white and 0 for black", () => {
    expect(relativeLuminance("#ffffff")).toBeCloseTo(1, 5);
    expect(relativeLuminance("#000000")).toBe(0);
  });

  it("supports #rgb shorthand", () => {
    expect(relativeLuminance("#fff")).toBeCloseTo(1, 5);
    expect(relativeLuminance("#000")).toBe(0);
  });

  it("supports 8-digit #rrggbbaa and 4-digit #rgba, ignoring alpha", () => {
    // Alpha byte must not change the luminance (opaque-color property).
    expect(relativeLuminance("#ffffff00")).toBeCloseTo(1, 5);
    expect(relativeLuminance("#ffffffff")).toBeCloseTo(1, 5);
    expect(relativeLuminance("#00000080")).toBe(0);
    expect(relativeLuminance("#000f")).toBe(0); // #rgba → #000 opaque
    // Same rgb, different alpha → identical luminance.
    expect(relativeLuminance("#0b0b0bff")).toBe(relativeLuminance("#0b0b0b"));
  });

  it("tolerates a missing leading #", () => {
    expect(relativeLuminance("ffffff")).toBeCloseTo(1, 5);
  });

  it("returns 1 (treat-as-bright) for a malformed or non-hex value", () => {
    expect(relativeLuminance("garbage")).toBe(1);
    expect(relativeLuminance("")).toBe(1);
    expect(relativeLuminance("#12")).toBe(1);
    expect(relativeLuminance("#12345")).toBe(1); // 5 digits — not a valid width
    expect(relativeLuminance("#1234567")).toBe(1); // 7 digits — not a valid width
    expect(relativeLuminance("#zzzzzz")).toBe(1);
    expect(relativeLuminance("rgb(11,11,11)")).toBe(1); // non-hex form → not parsed
    expect(relativeLuminance("var(--danger)")).toBe(1);
  });

  it("orders mid colors between black and white", () => {
    const l = relativeLuminance("#808080");
    expect(l).toBeGreaterThan(0);
    expect(l).toBeLessThan(1);
  });
});
