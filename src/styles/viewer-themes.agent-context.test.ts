// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { BUNDLED_THEMES, themeTokens } from "../themes";
import { contrastRatio, CONTRAST_THRESHOLDS } from "../shared/lib/color-contrast";
const css = readFileSync(resolve(__dirname, "viewer-themes.css"), "utf8");
describe("muted agent-context rows", () => {
  for (const id of Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>)
    for (const mode of ["light", "dark"] as const)
      it(`${id}/${mode} stays readable and secondary`, () => {
        const t = themeTokens(id, mode),
          bg = t["--theme-surface-sidebar-bg"],
          out = t["--theme-compat-cs-agent-ctx-out"],
          text = t["--theme-text-sidebar-primary"];
        expect(contrastRatio(out, bg)).toBeGreaterThanOrEqual(CONTRAST_THRESHOLDS.text);
        expect(contrastRatio(out, bg)).toBeLessThan(contrastRatio(text, bg));
      });
  it("uses the shared semantic alias for the overlay and source tag", () => {
    expect(css).toMatch(
      /\.cs-agent-ctx-out \.cs-sidebar-item,\s*\.cs-agent-ctx-tag \{\s*color: var\(\s*--cs-agent-ctx-out/,
    );
  });
});
