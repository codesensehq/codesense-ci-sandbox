// SPDX-License-Identifier: MIT
import {
  parseHexColor,
  relativeLuminance,
  toHexColor as toHex,
  toOklab,
} from "@/shared/lib/color-contrast";
import { useMachineAppearanceStore } from "./machine-appearance";

/** A remote machine's color choices. The palette stays clear of the status
 *  hues (running green, error red) and of the amber that means offline or
 *  "needs you". Names are what a screen reader hears on the swatches. */
export const MACHINE_PALETTE = [
  { name: "Blue", hex: "#58a6ff" },
  { name: "Purple", hex: "#bc8cff" },
  { name: "Orange", hex: "#f0883e" },
  { name: "Teal", hex: "#39c5cf" },
  { name: "Pink", hex: "#f778ba" },
  { name: "Mint", hex: "#56d4bc" },
  { name: "Periwinkle", hex: "#8b9eff" },
  { name: "Sky", hex: "#a5d6ff" },
] as const;

/** The color a machine wears until its user picks one: derived from its id,
 *  so it never changes between launches. */
export function defaultMachineColor(machineId: string): string {
  let hash = 0;
  for (let i = 0; i < machineId.length; i++) {
    hash = (hash * 31 + machineId.charCodeAt(i)) >>> 0;
  }
  return MACHINE_PALETTE[hash % MACHINE_PALETTE.length].hex;
}

/** A remote machine's color: the same everywhere it appears (workspace pill,
 *  tab glyph, terminal tint, machine card) — the user's pick, else its
 *  default. Outside React; components use {@link useMachineColor}. */
export function machineColor(machineId: string): string {
  return (
    useMachineAppearanceStore.getState().prefs[machineId]?.color ?? defaultMachineColor(machineId)
  );
}

/** {@link machineColor}, re-rendering when the user changes it. */
export function useMachineColor(machineId: string): string {
  const color = useMachineAppearanceStore((s) => s.prefs[machineId]?.color);
  return color ?? defaultMachineColor(machineId);
}

/** Whether a background is dark (a mark over it should lighten, not darken). */
export function isDarkColor(hex: string): boolean {
  return relativeLuminance(hex) < 0.18;
}

// --- OKLCH (Björn Ottosson's OKLab), for the custom-color lightness clamp ---

const unlin = (c: number) =>
  255 * (c <= 0.0031308 ? 12.92 * c : 1.055 * Math.max(0, c) ** (1 / 2.4) - 0.055);

function fromOklab([L, A, B]: [number, number, number]): [number, number, number] {
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.291485548 * B) ** 3;
  return [
    unlin(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
    unlin(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
    unlin(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s),
  ];
}

/** OKLCH lightness bounds a custom machine color is held to: the band the
 *  palette itself spans (0.715 blue … 0.856 sky), widened a little downward,
 *  so a custom glyph reads like the palette's on the dark terminal chrome and
 *  a near-black or near-white pick still tints visibly. */
export const CUSTOM_LIGHTNESS = { min: 0.6, max: 0.86 } as const;

/** A user's custom pick, its OKLCH lightness clamped into
 *  {@link CUSTOM_LIGHTNESS} (hue and chroma kept). Lowercase `#rrggbb`;
 *  `null` for a value that is not a hex color. */
export function clampMachineColor(hex: string): string | null {
  const rgb = parseHexColor(hex);
  if (!rgb) return null;
  const [L, A, B] = toOklab(rgb);
  const clamped = Math.min(CUSTOM_LIGHTNESS.max, Math.max(CUSTOM_LIGHTNESS.min, L));
  if (clamped === L) return toHex(rgb);
  return toHex(fromOklab([clamped, A, B]));
}

/** The OKLCH lightness of a hex color (0..1), for tests and the clamp. */
export function oklchLightness(hex: string): number {
  const rgb = parseHexColor(hex);
  return rgb ? toOklab(rgb)[0] : 0;
}

/** `top` at `alpha` composited over `bg` with a `lighten` blend (per channel,
 *  the brighter of the two) — how a mark or image lands on a dark terminal. */
export function lightenOver(bg: string, top: string, alpha: number): string {
  const b = parseHexColor(bg);
  const t = parseHexColor(top);
  if (!b || !t) return bg;
  return toHex(
    [0, 1, 2].map((i) => b[i] + (Math.max(b[i], t[i]) - b[i]) * alpha) as [number, number, number],
  );
}

/** `top` at `alpha` composited over `bg` with a `multiply` blend — how a mark
 *  lands on a light terminal. */
export function multiplyOver(bg: string, top: string, alpha: number): string {
  const b = parseHexColor(bg);
  const t = parseHexColor(top);
  if (!b || !t) return bg;
  return toHex(
    [0, 1, 2].map((i) => b[i] + ((b[i] * t[i]) / 255 - b[i]) * alpha) as [number, number, number],
  );
}

export { mixHex, contrastRatio } from "@/shared/lib/color-contrast";
