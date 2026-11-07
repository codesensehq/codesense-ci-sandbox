// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { contrastRatio, CONTRAST_THRESHOLDS } from "../shared/lib/color-contrast";
import { BUNDLED_THEMES, themeTokens } from "./index";
import { SURFACES } from "./compiler";
import { kitRoles } from "./kit";
it("resolves every shared kit role in every family, mode and surface", () => {
  for (const id of Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>)
    for (const mode of ["light", "dark"] as const)
      for (const surface of SURFACES) {
        const tokens = themeTokens(id, mode);
        for (const [name, role] of Object.entries(kitRoles(surface)))
          expect(tokens[`--theme-${role}`], `${id}/${mode}/${surface}/${name}`).toBeTruthy();
      }
});

const themeIds = Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>;
const resolve = (id: (typeof themeIds)[number], mode: "light" | "dark", name: string) => {
  const tokens = themeTokens(id, mode);
  const roles = kitRoles("viewer", (role) => `--theme-${role}` in tokens);
  return tokens[`--theme-${roles[name]}`].toLowerCase();
};
const hue = (hex: string) => {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const max = Math.max(r, g, b);
  const d = max - Math.min(r, g, b);
  if (d === 0) return 0;
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return (h * 60 + 360) % 360;
};
const hueGap = (a: string, b: string) => {
  const gap = Math.abs(hue(a) - hue(b));
  return Math.min(gap, 360 - gap);
};
const distinctPairs = [
  ["story-review", "story-done"],
  ["phase-review", "phase-done"],
  ["phase-pr", "phase-done"],
] as const;
describe.each(themeIds)("%s review hue", (id) => {
  it.each(["light", "dark"] as const)(
    "%s keeps review and PR a different hue from done",
    (mode) => {
      for (const [a, b] of distinctPairs)
        expect(
          hueGap(resolve(id, mode, a), resolve(id, mode, b)),
          `${a} vs ${b}`,
        ).toBeGreaterThanOrEqual(40);
    },
  );
  it.each(["light", "dark"] as const)("%s review stays legible on the viewer surface", (mode) => {
    const bg = themeTokens(id, mode)["--theme-surface-viewer-bg"];
    expect(contrastRatio(resolve(id, mode, "story-review"), bg)).toBeGreaterThanOrEqual(
      CONTRAST_THRESHOLDS.nonText,
    );
  });
});

it("only themes that define review.status move review and PR off the function hue", () => {
  for (const id of themeIds)
    for (const mode of ["light", "dark"] as const) {
      const tokens = themeTokens(id, mode);
      const expected =
        id === "dracula" ? tokens["--theme-review-status"] : tokens["--theme-code-function"];
      for (const name of ["story-review", "phase-review", "phase-pr"])
        expect(resolve(id, mode, name), `${id}/${mode}/${name}`).toBe(expected.toLowerCase());
    }
});
