// SPDX-License-Identifier: MIT
import { expect, it } from "vitest";
import { APP_ALIASES, surfaceCss } from "./css";
import { compositeOver, contrastRatio, CONTRAST_THRESHOLDS } from "../shared/lib/color-contrast";
import { BUNDLED_THEMES, themeTokens, type AppTheme } from "./index";

/** The `--cs-runtime-*` aliases, as theme roles per surface (see css.ts). */
const RUNTIMES = {
  claude: (s: string) => [`severitySurface-${s}-warning-fg`, `severitySurface-${s}-warning-bg`],
  codex: (s: string) => [`severitySurface-${s}-info-fg`, `severitySurface-${s}-info-bg`],
  opencode: (s: string) => [`text-${s}-muted`, `surface-${s}-selected`],
  hermes: (s: string) => [`runtime-hermes-${s}-text`, `runtime-hermes-${s}-bg`],
};
const SURFACES = ["canvas", "sidebar", "viewer"];

/** The chip is opaque (agents.css): tint over the surface bg. A row's hover/selected/muted
 *  fill sits behind that opaque layer, so it cannot change the painted pixels. */
export function runtimeBadgeContrast(id: AppTheme, mode: "light" | "dark") {
  const t = themeTokens(id, mode);
  const rows: { name: string; ratio: number }[] = [];
  for (const s of SURFACES) {
    const base = t[`--theme-surface-${s}-bg`];
    const parents = [
      ["base", base],
      ["hover", compositeOver(t[`--theme-surface-${s}-hover`], base)],
      ["selected", compositeOver(t[`--theme-surface-${s}-selected`], base)],
    ] as const;
    for (const [runtime, roles] of Object.entries(RUNTIMES)) {
      const [fg, bg] = roles(s);
      for (const [state, parent] of parents) {
        const paint = compositeOver(compositeOver(t[`--theme-${bg}`], base), parent);
        rows.push({
          name: `${id}/${mode}/${s}/${runtime}/on-${state}`,
          ratio: contrastRatio(t[`--theme-${fg}`], paint),
        });
      }
    }
  }
  return rows;
}

const themes = (Object.keys(BUNDLED_THEMES) as AppTheme[]).flatMap((id) =>
  (["light", "dark"] as const).map((mode) => [id, mode] as const),
);

it.each(themes)("%s/%s runtime badges clear AA on every surface and row state", (id, mode) => {
  for (const { name, ratio } of runtimeBadgeContrast(id, mode))
    expect(ratio, name).toBeGreaterThanOrEqual(CONTRAST_THRESHOLDS.text);
});

it.each(themes)("%s/%s Hermes has an opaque tinted pair distinct from the surface", (id, mode) => {
  const t = themeTokens(id, mode);
  for (const surface of SURFACES) {
    expect(t[`--theme-runtime-hermes-${surface}-bg`]).not.toBe(t[`--theme-surface-${surface}-bg`]);
    expect(t[`--theme-runtime-hermes-${surface}-bg`]).toMatch(/^#[0-9a-f]{6}$/i);
    expect(t[`--theme-runtime-hermes-${surface}-text`]).toMatch(/^#[0-9a-f]{6}$/i);
  }
});

it("Hermes CSS aliases use the same opaque pair measured by the contrast test", () => {
  expect(APP_ALIASES["cs-runtime-hermes-text"]).toBe("runtime-hermes-canvas-text");
  expect(APP_ALIASES["cs-runtime-hermes-bg"]).toBe("runtime-hermes-canvas-bg");
  const css = surfaceCss();
  for (const surface of SURFACES) {
    expect(css).toContain(`--cs-runtime-hermes-text: var(--theme-runtime-hermes-${surface}-text);`);
    expect(css).toContain(`--cs-runtime-hermes-bg: var(--theme-runtime-hermes-${surface}-bg);`);
  }
});
