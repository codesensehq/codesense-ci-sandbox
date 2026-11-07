// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it } from "vitest";
import {
  CUSTOM_LIGHTNESS,
  MACHINE_PALETTE,
  clampMachineColor,
  defaultMachineColor,
  machineColor,
  mixHex,
  oklchLightness,
} from "./machine-color";
import { __resetMachineAppearanceForTest, setMachineColor } from "./machine-appearance";

afterEach(() => __resetMachineAppearanceForTest());

describe("machineColor", () => {
  it("gives a machine the same color every time", () => {
    expect(machineColor("codesense-test-1-private-2fa19f")).toBe(
      machineColor("codesense-test-1-private-2fa19f"),
    );
  });

  it("spreads different machines across the palette", () => {
    const colors = new Set(
      ["box-a", "box-b", "box-c", "box-d", "box-e", "box-f"].map(machineColor),
    );
    expect(colors.size).toBeGreaterThan(2);
    for (const c of colors) expect(MACHINE_PALETTE.map((p) => p.hex)).toContain(c);
  });

  it("is the user's pick when there is one, else the default", () => {
    expect(machineColor("box-a")).toBe(defaultMachineColor("box-a"));
    setMachineColor("box-a", "#123456");
    expect(machineColor("box-a")).toBe("#123456");
    setMachineColor("box-a", null);
    expect(machineColor("box-a")).toBe(defaultMachineColor("box-a"));
  });
});

describe("mixHex", () => {
  it("moves a color part of the way toward another", () => {
    expect(mixHex("#000000", "#ffffff", 0)).toBe("#000000");
    expect(mixHex("#000000", "#ffffff", 1)).toBe("#ffffff");
    expect(mixHex("#000000", "#ff0000", 0.5)).toBe("#800000");
  });
  it("leaves a non-hex base as it was", () => {
    expect(mixHex("var(--x)", "#ffffff", 0.5)).toBe("var(--x)");
  });
});

describe("clampMachineColor", () => {
  it("keeps a legible pick as it is", () => {
    expect(clampMachineColor("#39C5CF")).toBe("#39c5cf");
  });
  it("lifts a near-black pick and dims a near-white one into the legible band", () => {
    for (const pick of ["#050510", "#000000", "#fefefe", "#ffffff", "#ffff00"]) {
      const out = clampMachineColor(pick)!;
      expect(out).toMatch(/^#[0-9a-f]{6}$/);
      const l = oklchLightness(out);
      expect(l).toBeGreaterThanOrEqual(CUSTOM_LIGHTNESS.min - 0.02);
      expect(l).toBeLessThanOrEqual(CUSTOM_LIGHTNESS.max + 0.02);
    }
  });
  it("refuses a value that isn't a color", () => {
    expect(clampMachineColor("blue")).toBeNull();
  });
});
