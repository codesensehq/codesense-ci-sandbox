// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { CircleDashed, Circle, Ban } from "lucide-react";
import { resolveLucideIcon, knownLucideIconNames, FALLBACK_LUCIDE_ICON } from "./lucide-icon-map";

describe("resolveLucideIcon (SW-GT-3 · AC-4)", () => {
  it("resolves a known name to its named-imported component", () => {
    expect(resolveLucideIcon("CircleDashed")).toBe(CircleDashed);
    expect(resolveLucideIcon("Ban")).toBe(Ban);
  });

  it("includes all 7 default-taxonomy icons", () => {
    for (const name of [
      "CircleDashed",
      "CircleDot",
      "Circle",
      "Ban",
      "Eye",
      "ShieldCheck",
      "CheckCircle2",
    ]) {
      expect(knownLucideIconNames()).toContain(name);
    }
  });

  it("falls back (never crashes/blanks) for an unknown or typo'd name", () => {
    expect(resolveLucideIcon("NotARealIcon")).toBe(FALLBACK_LUCIDE_ICON);
    expect(resolveLucideIcon("circledashed")).toBe(FALLBACK_LUCIDE_ICON); // case-sensitive
  });

  it("falls back for null / undefined / empty", () => {
    expect(resolveLucideIcon(null)).toBe(FALLBACK_LUCIDE_ICON);
    expect(resolveLucideIcon(undefined)).toBe(FALLBACK_LUCIDE_ICON);
    expect(resolveLucideIcon("")).toBe(FALLBACK_LUCIDE_ICON);
  });

  it("the fallback is a real component (a neutral circle)", () => {
    expect(FALLBACK_LUCIDE_ICON).toBe(Circle);
  });
});
