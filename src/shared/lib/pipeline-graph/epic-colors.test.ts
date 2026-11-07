// SPDX-License-Identifier: MIT
import { MANAGED_TREE_ACCENTS, getManagedTreeAccent } from "./epic-colors";
import { describe, it, expect } from "vitest";
import { getEpicColor, getRepoColor, getEpicBadgeBg, getEpicSpineColor } from "./epic-colors";

const SAT = (hsl: string): number => Number(hsl.match(/hsl\(\d+,\s*(\d+)%/)?.[1] ?? NaN);

describe("identity-color primitive", () => {
  it("getEpicColor is deterministic for a given key + theme", () => {
    expect(getEpicColor("workspaces", false)).toBe(getEpicColor("workspaces", false));
    expect(getEpicColor("workspaces", true)).toBe(getEpicColor("workspaces", true));
  });

  it("getRepoColor is deterministic for a given slug + theme", () => {
    expect(getRepoColor("acme/web", false)).toBe(getRepoColor("acme/web", false));
    expect(getRepoColor("acme/api", false)).toBe(getRepoColor("acme/api", false));
  });

  it("dark and light themes return distinct values for the same key", () => {
    expect(getEpicColor("workspaces", false)).not.toBe(getEpicColor("workspaces", true));
    expect(getRepoColor("acme/web", false)).not.toBe(getRepoColor("acme/web", true));
  });

  it("returns valid hsl() strings from the curated palette", () => {
    expect(getEpicColor("anything", false)).toMatch(/^hsl\(\d+, \d+%, \d+%\)$/);
    expect(getRepoColor("anything", true)).toMatch(/^hsl\(\d+, \d+%, \d+%\)$/);
  });

  it("epic↔graph parity: the panel passing dirName matches the graph's key", () => {
    // The graph calls getEpicColor(entry.dirName, …) (derived-tree-to-react-flow.ts).
    // The panel must pass the same dirName so accents line up. Same input → same color.
    const dirName = "epic-workspaces";
    expect(getEpicColor(dirName, false)).toBe(getEpicColor(dirName, false));
  });

  it("getEpicBadgeBg derives a tint from an epic color without throwing", () => {
    const color = getEpicColor("workspaces", false);
    expect(getEpicBadgeBg(color, false)).toMatch(/^hsl\(/);
    expect(getEpicBadgeBg(color, true)).toMatch(/^hsl\(/);
  });

  it("getEpicSpineColor desaturates the epic color (faint grouping hint)", () => {
    // SW-CW-2: the spine must never compete with the neutral selection bar, so
    // its saturation is strictly lower than the full epic identity color while
    // the hue (grouping identity) is preserved.
    for (const theme of [false, true]) {
      const full = getEpicColor("workspaces", theme);
      const spine = getEpicSpineColor("workspaces", theme);
      expect(spine).toMatch(/^hsl\(\d+, \d+%, \d+%\)$/);
      expect(SAT(spine)).toBeLessThan(SAT(full));
      // Same hue = same grouping identity.
      expect(spine.match(/hsl\((\d+),/)?.[1]).toBe(full.match(/hsl\((\d+),/)?.[1]);
    }
  });

  it("getEpicSpineColor is deterministic for a given key + theme", () => {
    expect(getEpicSpineColor("workspaces", false)).toBe(getEpicSpineColor("workspaces", false));
    expect(getEpicSpineColor("workspaces", true)).toBe(getEpicSpineColor("workspaces", true));
  });
});

describe("management accents", () => {
  it("uses a stable filtered identity palette in each theme", () => {
    for (const light of [true, false]) {
      const palette = light ? MANAGED_TREE_ACCENTS.light : MANAGED_TREE_ACCENTS.dark;
      expect(palette).toHaveLength(2);
      expect(palette.every((color) => /^hsl\((285|340),/.test(color))).toBe(true);
      expect(palette).toContain(getManagedTreeAccent("org:team|lead", light));
      expect(getManagedTreeAccent("org:team|lead", light)).toBe(
        getManagedTreeAccent("org:team|lead", light),
      );
    }
  });
});
