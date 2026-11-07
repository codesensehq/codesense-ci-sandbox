// SPDX-License-Identifier: MIT
import type { FontSize, TerminalFontSize, TreeFontSize } from "@/shared/stores/viewer-store";

// Preset → pixel values. Single source of truth for viewer and
// terminal preset font sizes; everything that renders at runtime goes
// through the helpers below.
export const VIEWER_FONT_PX_MAP: Record<FontSize, number> = {
  small: 12,
  normal: 14,
  large: 16,
  xl: 18,
};

export const TERMINAL_FONT_PX_MAP: Record<TerminalFontSize, number> = {
  small: 11,
  normal: 13,
  large: 15,
  xl: 17,
};

export const TREE_FONT_PX_MAP: Record<TreeFontSize, number> = {
  small: 11,
  normal: 12,
  large: 13,
};

// SS-14: shared <CodeEditor>'s 4th preset domain (D-13). Scaled to match
// terminal — code-editor and terminal are the closest visual peers in the
// app and benefit from matching defaults.
export const EDITOR_FONT_PX_MAP: Record<FontSize, number> = {
  small: 11,
  normal: 13,
  large: 15,
  xl: 17,
};

// Minimum/maximum the UI input will accept. Anything outside this range
// is clamped before being written to settings so stray keystrokes can't
// brick the viewer or terminal rendering.
export const FONT_PX_MIN = 8;
export const FONT_PX_MAX = 40;

export function clampFontPx(n: number): number {
  if (!Number.isFinite(n)) return 0;
  const rounded = Math.round(n);
  if (rounded < FONT_PX_MIN) return FONT_PX_MIN;
  if (rounded > FONT_PX_MAX) return FONT_PX_MAX;
  return rounded;
}

export function getEffectiveViewerFontPx(preset: FontSize, customPx: number | undefined): number {
  if (customPx && customPx > 0) return customPx;
  return VIEWER_FONT_PX_MAP[preset] ?? VIEWER_FONT_PX_MAP.large;
}

export function getEffectiveTerminalFontPx(
  preset: TerminalFontSize,
  customPx: number | undefined,
): number {
  if (customPx && customPx > 0) return customPx;
  return TERMINAL_FONT_PX_MAP[preset] ?? TERMINAL_FONT_PX_MAP.large;
}

export function getEffectiveTreeFontPx(preset: TreeFontSize, customPx: number | undefined): number {
  if (customPx && customPx > 0) return customPx;
  return TREE_FONT_PX_MAP[preset] ?? TREE_FONT_PX_MAP.large;
}

export function getEffectiveCodeEditorFontPx(
  preset: FontSize,
  customPx: number | undefined,
): number {
  if (customPx && customPx > 0) return customPx;
  return EDITOR_FONT_PX_MAP[preset] ?? EDITOR_FONT_PX_MAP.normal;
}
