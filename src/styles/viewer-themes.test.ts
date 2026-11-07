// SPDX-License-Identifier: MIT
import { compile as parseStylesheet } from "tailwindcss";
import { describe, expect, it } from "vitest";
import { resolve } from "node:path";
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { BUNDLED_THEMES, themeTokens } from "../themes";
import { APP_ALIASES } from "../themes/css";
import { SURFACES } from "../themes/compiler";
import { contrastPairs, terminalPairs } from "../themes/contrast";
import { TERMINAL_THEMES } from "../features/viewer/components/terminal-constants";
import {
  compositeOver,
  contrastRatio,
  oklabDistance,
  CONTRAST_THRESHOLDS,
} from "../shared/lib/color-contrast";

const appCss = readFileSync(resolve(__dirname, "../index.css"), "utf8");
const css = readFileSync(resolve(__dirname, "theme-tokens.css"), "utf8");
const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((block) => ({
  selector: block[1].replace(/\s+/g, " "),
  entries: [...block[2].matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(([, k, v]) => [k, v.trim()]),
}));
function cascade(root: Record<string, string>, surface: string) {
  const result = { ...root };
  for (const rule of rules) {
    if (
      !rule.selector.includes(":root, .dark") &&
      !rule.selector.trim().startsWith(`[data-surface="${surface}"]`)
    )
      continue;
    for (const [k, v] of rule.entries) result[k] = v;
  }
  const get = (name: string): string =>
    result[name]?.replace(/var\((--[\w-]+)\)/g, (_, k) => get(k));
  return get;
}
it("generated stylesheets parse as complete CSS", async () => {
  await expect(parseStylesheet(css)).resolves.toBeDefined();
  const kit = readFileSync(
    resolve(__dirname, "../../src-tauri/src/http/routes/dashboard_kit/tokens.css"),
    "utf8",
  );
  await expect(parseStylesheet(kit)).resolves.toBeDefined();
});
describe("bundled rendered theme cascade", () => {
  let count = 0;
  for (const id of Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>)
    for (const mode of ["light", "dark"] as const) {
      it(`${id}/${mode}: all design contrast pairs and surface resets`, () => {
        const tokens = themeTokens(id, mode);
        const pairs = contrastPairs(tokens, mode, id === "codesense" && mode === "light");
        count += pairs.length;
        expect(pairs.filter((p) => p.ratio < p.min)).toEqual([]);
        for (const s of SURFACES) {
          const get = cascade(tokens, s);
          expect(get("--foreground")).toBe(tokens[`--theme-text-${s}-primary`]);
          expect(get("--cs-warning-text")).toBe(tokens[`--theme-severitySurface-${s}-warning-fg`]);
          expect(get("--destructive")).toBe(tokens[`--theme-severitySurface-${s}-danger-fg`]);
          for (const state of ["bg", "hover", "selected"]) {
            const bg = compositeOver(
              tokens[`--theme-surface-${s}-${state}`],
              tokens[`--theme-surface-${s}-bg`],
            );
            for (const name of ["needs", "failed", "busy", "done", "idle"])
              expect(contrastRatio(get(`--cs-status-${name}`), bg)).toBeGreaterThanOrEqual(
                CONTRAST_THRESHOLDS.nonText,
              );
          }
          // app-painted children explicitly reset after a terminal/sidebar parent.
          const child = cascade(tokens, "canvas");
          expect(child("--foreground")).toBe(tokens["--theme-text-canvas-primary"]);
          const warningBg = compositeOver(get("--cs-warning-bg"), get("--background"));
          expect(contrastRatio(get("--cs-warning-text"), warningBg)).toBeGreaterThanOrEqual(
            CONTRAST_THRESHOLDS.text,
          );
          if (s !== "terminal")
            expect(contrastRatio(get("--input"), get("--background"))).toBeGreaterThanOrEqual(
              CONTRAST_THRESHOLDS.nonText,
            );
        }
      });
    }
  it("covers the 2,162 design pairs including the five explicit terminal palettes", () => {
    for (const t of Object.values(TERMINAL_THEMES)) {
      const { theme: p } = t;
      const names = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "brightBlack",
        "brightRed",
        "brightGreen",
        "brightYellow",
        "brightBlue",
        "brightMagenta",
        "brightCyan",
        "brightWhite",
      ] as const;
      const pairs = terminalPairs({
        bg: p.background!,
        fg: p.foreground!,
        cursor: p.cursor!,
        selection: p.selectionBackground!,
        ...Object.fromEntries(names.map((n, i) => [`ansi-${i}`, p[n]!])),
      });
      count += pairs.length;
      expect(pairs.filter((p) => p.ratio < p.min)).toEqual([]);
    }
    expect(count).toBe(2162);
  });
  it("reports severity distinctness and ANSI on selection without gating", () => {
    const report = [];
    for (const id of Object.keys(BUNDLED_THEMES) as Array<keyof typeof BUNDLED_THEMES>)
      for (const mode of ["light", "dark"] as const) {
        const t = themeTokens(id, mode),
          sev = ["info", "success", "warning", "danger"];
        let nearest = Infinity;
        for (let i = 0; i < sev.length; i++)
          for (let j = i + 1; j < sev.length; j++)
            nearest = Math.min(
              nearest,
              oklabDistance(t[`--theme-severity-${sev[i]}-fg`], t[`--theme-severity-${sev[j]}-fg`]),
            );
        const selection = compositeOver(t["--theme-terminal-selection"], t["--theme-terminal-bg"]);
        report.push({
          theme: id,
          mode,
          severityDeltaE: nearest.toFixed(3),
          minAnsiOnSelection: Math.min(
            ...Array.from({ length: 15 }, (_, i) =>
              contrastRatio(t[`--theme-terminal-ansi-${i + 1}`], selection),
            ),
          ).toFixed(2),
        });
      }
    console.info("Report only:", report);
  });
  it("ships generated defaults and surface rules without stale outputs", () => {
    for (const [name, role] of Object.entries(APP_ALIASES))
      expect(css).toContain(`--${name}: var(--theme-${role});`);
    expect(appCss).toContain('@import "./styles/theme-tokens.css"');
    execFileSync("pnpm", ["gen:theme:check"], { cwd: process.cwd() });
  });
});
