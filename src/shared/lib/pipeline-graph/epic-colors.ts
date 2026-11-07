// SPDX-License-Identifier: MIT
/**
 * Deterministic identity-color primitive (SW-EW-1 AC-9).
 *
 * The shared owner of "stable id → color" mappings used across the pipeline
 * graph and the CLI workspace management panel:
 *   - `getEpicColor(epicDirName)` — epic accent / cross-epic dependency edges.
 *   - `getRepoColor(repoSlug)`    — repo-glyph color on workspace pills.
 *
 * Each id is hashed into a curated 10-color palette. Because the panel keys
 * epics on the SAME `epicDirName` the graph uses, an epic's panel accent
 * matches its graph color — one teal epic everywhere. EW-3 (tile stripes)
 * consumes this same primitive.
 *
 * Two palettes (dark/light) ensure readability across all viewer themes.
 * Hues are chosen to avoid collision with the 7 status colors
 * (slate, blue, red, purple, amber, green, sky).
 */

// Dark themes: saturated, mid-high lightness (L60-65%) for visibility on
// dark canvases (GitHub Dark L~7% through Nord L~21%).
const DARK_PALETTE = [
  "hsl(175, 55%, 55%)", // teal
  "hsl(340, 55%, 65%)", // rose
  "hsl(240, 50%, 68%)", // indigo
  "hsl(25, 60%, 60%)", // tangerine
  "hsl(100, 45%, 55%)", // lime
  "hsl(310, 50%, 65%)", // magenta
  "hsl(195, 60%, 55%)", // cyan
  "hsl(15, 60%, 65%)", // coral
  "hsl(285, 45%, 65%)", // lavender
  "hsl(70, 50%, 50%)", // chartreuse
] as const;

// Light themes (Confluence): same hues, lower lightness (L35-45%)
// for readability on white/light canvases.
const LIGHT_PALETTE = [
  "hsl(175, 55%, 35%)", // teal
  "hsl(340, 55%, 45%)", // rose
  "hsl(240, 50%, 45%)", // indigo
  "hsl(25, 60%, 42%)", // tangerine
  "hsl(100, 45%, 35%)", // lime
  "hsl(310, 50%, 45%)", // magenta
  "hsl(195, 60%, 35%)", // cyan
  "hsl(15, 60%, 45%)", // coral
  "hsl(285, 45%, 45%)", // lavender
  "hsl(70, 50%, 35%)", // chartreuse
] as const;

/** Simple stable hash of a string to a non-negative integer. */
function hashString(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) - h + s.charCodeAt(i)) | 0;
  }
  return Math.abs(h);
}

/**
 * Get the epic color for a given epic directory name.
 * Deterministic: same name always produces the same color index.
 *
 * @param epicDirName - The epic's directory name (e.g., "workspaces", "pipeline")
 * @param isLightTheme - True for light themes (Confluence), false for dark themes
 */
export function getEpicColor(epicDirName: string, isLightTheme: boolean): string {
  const palette = isLightTheme ? LIGHT_PALETTE : DARK_PALETTE;
  const index = hashString(epicDirName) % palette.length;
  return palette[index];
}

/**
 * Get the repo-glyph color for a given source-repo slug (SW-EW-1 AC-9).
 * Deterministic: same slug always produces the same color. Shares the
 * mechanism + palettes with `getEpicColor` so repo identity reads in the
 * same visual language as epic identity. The glyph TEXT is owned separately
 * by `computeTeamGlyphMap` (team-key.ts); this supplies only the color.
 *
 * @param repoSlug - The source-repo slug (e.g., "acme/web").
 * @param isLightTheme - True for light themes (Confluence), false for dark.
 */
export function getRepoColor(repoSlug: string, isLightTheme: boolean): string {
  const palette = isLightTheme ? LIGHT_PALETTE : DARK_PALETTE;
  const index = hashString(repoSlug) % palette.length;
  return palette[index];
}

/** Parse an `hsl(h, s%, l%)` string into numeric components, or null if it
 *  doesn't match. Shared by the epic-color derivations below so the parse lives
 *  in one place. */
function parseHsl(hsl: string): { h: number; s: number; l: number } | null {
  const match = hsl.match(/hsl\((\d+),\s*(\d+)%,\s*(\d+)%\)/);
  if (!match) return null;
  return { h: Number(match[1]), s: Number(match[2]), l: Number(match[3]) };
}

/**
 * Get a low-opacity tint of the epic color for badge backgrounds.
 * Uses the same hue as the edge stroke but at very low opacity.
 *
 * @param epicColor - The epic's stroke color (from getEpicColor)
 * @param isLightTheme - True for light themes
 */
export function getEpicBadgeBg(epicColor: string, isLightTheme: boolean): string {
  // Extract HSL values and return a low-opacity version.
  const parsed = parseHsl(epicColor);
  if (!parsed) return epicColor;
  const { h, s } = parsed;
  // Dark: low lightness tint. Light: high lightness tint.
  if (isLightTheme) {
    return `hsl(${h}, ${s}%, 92%)`;
  }
  return `hsl(${h}, ${s - 15}%, 20%)`;
}

/**
 * SW-CW-2: a faint, desaturated form of the epic color for the workspace-tray
 * grouping spine. Same hue (grouping identity preserved) but the saturation is
 * cut so the spine reads as a quiet hint and never competes with the neutral
 * selection bar (which is the sole full-contrast selection signal). Shares
 * `getEpicColor`'s palette/hash so the spine stays epic↔graph consistent.
 *
 * @param epicDirName - The epic's directory name.
 * @param isLightTheme - True for light themes (Confluence), false for dark.
 */
export function getEpicSpineColor(epicDirName: string, isLightTheme: boolean): string {
  const full = getEpicColor(epicDirName, isLightTheme);
  const parsed = parseHsl(full);
  if (!parsed) return full;
  const { h, s, l } = parsed;
  // Cut saturation to ~40% of the identity value (rounded) so the hue is still
  // discernible as a grouping hint but is visibly muted vs. the full color. The
  // `s - 1` clamp keeps the spine STRICTLY less saturated than the identity for
  // any palette entry (holds even if a future color has a very low saturation).
  const mutedS = Math.min(Math.round(s * 0.4), Math.max(s - 1, 0));
  return `hsl(${h}, ${mutedS}%, ${l}%)`;
}

/** Management guides use only the identity hues clear of the lifecycle marks. */
export const MANAGED_TREE_ACCENTS = {
  dark: DARK_PALETTE.filter((color) => [285, 340].includes(parseHsl(color)!.h)),
  light: LIGHT_PALETTE.filter((color) => [285, 340].includes(parseHsl(color)!.h)),
} as const;

export function getManagedTreeAccent(treeKey: string, isLightTheme: boolean): string {
  const palette = isLightTheme ? MANAGED_TREE_ACCENTS.light : MANAGED_TREE_ACCENTS.dark;
  return palette[hashString(treeKey) % palette.length];
}
