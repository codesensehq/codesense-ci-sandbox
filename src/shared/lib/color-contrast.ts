// SPDX-License-Identifier: MIT
/**
 * SW-GT-20 · Generic sRGB color helpers. Deliberately status-agnostic — takes any
 * hex — so any surface that must decide contrast against the dark app chrome can
 * reuse it. The workspace-pill status-icon boost is the first caller; agent-status
 * and tag colors are candidate future adopters (which is why this lives in
 * `shared/lib`, not the pipeline-graph status seam).
 */

/** Parse a hex color (leading `#` optional) to `[r,g,b]` in 0..255, or `null` when
 *  the string isn't a well-formed hex color. Accepts `#rgb`, `#rgba`, `#rrggbb`,
 *  and `#rrggbbaa`; any alpha channel is DROPPED — luminance is an opaque-color
 *  property and the pill composites over solid chrome. Non-hex CSS forms
 *  (`rgb(...)`, `hsl(...)`, `var(...)`) are not parsed → `null` → treated as bright
 *  (no boost); the `StatusState.color` contract is hex. */
export function parseHexColor(hex: string): [number, number, number] | null {
  const m = /^#?([0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.exec(hex.trim());
  if (!m) return null;
  // Expand shorthand (#rgb / #rgba → #rrggbb(aa)), then read the first three
  // channels; `slice(0, 6)` discards any alpha byte.
  const expanded =
    m[1].length <= 4
      ? m[1]
          .split("")
          .map((c) => c + c)
          .join("")
      : m[1];
  const n = Number.parseInt(expanded.slice(0, 6), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

/** Linearize one 0..255 sRGB channel per the WCAG 2.x definition. */
function linearizeChannel(channel: number): number {
  const s = channel / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

/**
 * WCAG 2.x relative luminance (0 = black, 1 = white) of a hex color (3/4/6/8-digit,
 * alpha ignored). A malformed / non-hex value returns `1` ("treat as bright") so a
 * bad value never reads as dark.
 */
export function relativeLuminance(hex: string): number {
  const rgb = parseHexColor(hex);
  if (!rgb) return 1;
  const [r, g, b] = rgb;
  return 0.2126 * linearizeChannel(r) + 0.7152 * linearizeChannel(g) + 0.0722 * linearizeChannel(b);
}

export const CONTRAST_THRESHOLDS = { text: 4.5, nonText: 3, ansi: 3, mutedDistinct: 1.2 } as const;

export function toHexColor(rgb: [number, number, number]): string {
  return (
    "#" +
    rgb
      .map((v) =>
        Math.round(Math.min(255, Math.max(0, v)))
          .toString(16)
          .padStart(2, "0"),
      )
      .join("")
  );
}

/** `base` moved `amount` (0..1) of the way to `toward`: an opaque tint, so
 *  the terminal's own renderer paints it as its real background. A value that
 *  is not hex leaves `base` as it was. */
export function mixHex(base: string, toward: string, amount: number): string {
  const a = parseHexColor(base);
  const b = parseHexColor(toward);
  if (!a || !b) return base;
  return toHexColor(
    [0, 1, 2].map((i) => a[i] + (b[i] - a[i]) * amount) as [number, number, number],
  );
}

/** WCAG contrast ratio between two hex colors (1 … 21). */
export function contrastRatio(a: string, b: string): number {
  const [hi, lo] = [relativeLuminance(a), relativeLuminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/** Source-over compositing in sRGB; the underlying surface must be opaque. */
export function compositeOver(top: string, under: string): string {
  const rgb = parseHexColor(top);
  const bg = parseHexColor(under);
  if (!rgb || !bg) throw new Error(`Expected sRGB hex colors: ${top}, ${under}`);
  const hex = top.replace(/^#/, "");
  const alpha =
    hex.length === 4
      ? parseInt(hex[3] + hex[3], 16) / 255
      : hex.length === 8
        ? parseInt(hex.slice(6), 16) / 255
        : 1;
  return toHexColor(rgb.map((v, i) => v * alpha + bg[i] * (1 - alpha)) as [number, number, number]);
}

/** OKLab distance reports how distinguishable two severity colours are. */
export function oklabDistance(a: string, b: string): number {
  const lab = (hex: string) => toOklab(parseHexColor(hex) ?? [0, 0, 0]);
  const x = lab(a),
    y = lab(b);
  return Math.hypot(...x.map((v, i) => v - y[i]));
}

export function toOklab(rgb: [number, number, number]): [number, number, number] {
  const [r, g, b] = rgb.map(linearizeChannel);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}
