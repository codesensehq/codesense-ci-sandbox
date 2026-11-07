// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, it } from "vitest";
import { BUNDLED_THEMES, themeTokens } from "@/themes";
import { contrastRatio, mixHex, relativeLuminance } from "@/shared/lib/color-contrast";
const css = readFileSync(resolve(__dirname, "../../../index.css"), "utf8");
it("keeps CodeSense tab bars darker than its title bar in both modes", () => {
  for (const mode of ["light", "dark"] as const) {
    const t = themeTokens("codesense", mode),
      title = t["--theme-surface-canvas-bg"];
    const bar = mixHex(
      mode === "dark" ? title : t["--theme-terminal-bg"],
      "#000000",
      mode === "dark" ? 0.8 : 0.3,
    );
    expect(relativeLuminance(bar)).toBeLessThan(relativeLuminance(title));
    expect(contrastRatio(bar, title)).toBeGreaterThanOrEqual(1.08);
  }
});
it("gives other light families light terminal surfaces and their dark modes dark surfaces", () => {
  for (const id of Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>) {
    expect(relativeLuminance(themeTokens(id, "dark")["--theme-surface-terminal-bg"])).toBeLessThan(
      0.4,
    );
    if (id !== "codesense")
      expect(
        relativeLuminance(themeTokens(id, "light")["--theme-surface-terminal-bg"]),
      ).toBeGreaterThan(0.4);
  }
  expect(css).toContain('[data-surface="terminal"]');
  expect(css).toContain("var(--cs-tile-terminal-bg, var(--theme-terminal-bg))");
});
