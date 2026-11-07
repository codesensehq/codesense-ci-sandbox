// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import {
  TERMINAL_THEMES,
  type TerminalThemeConfig,
} from "@/features/viewer/components/terminal-constants";
import { useMachineBackground } from "../lib/machine-appearance";
import { isDarkColor, mixHex, useMachineColor } from "../lib/machine-color";

/** How far a remote terminal's background moves toward its Machine's color:
 *  enough to tell the surface apart at a glance, never enough to cost text
 *  contrast. A light background takes less, or it turns muddy. */
export const PANE_TINT = { dark: 0.09, light: 0.06 } as const;
/** How far an inactive remote tab's chrome moves toward the Machine's color. */
export const TAB_TINT = 0.1;
/** The watermark's strength: a hint you notice when looking for it, never a
 *  shape competing with the text (see the contrast test). */
export const WATERMARK_OPACITY = { alone: 0.06, withImage: 0.045 } as const;
/** How a background image is dimmed over a dark terminal before its opacity
 *  applies, so its brightest pixel can't wash out dim text. */
export const IMAGE_DIM = { brightness: 0.55, saturate: 0.85 } as const;

/** What a Machine's watermark says: its host's name, without a trailing
 *  "(private)"-style note; its address when the label is only such a note. */
export function machineWatermarkText(label: string, host?: string | null): string {
  return label.replace(/\s*\([^)]*\)\s*$/, "").trim() || host || label;
}

const DEFAULT_THEME = TERMINAL_THEMES["github-dark"];

/** `bg` tinted toward `color` by the pane amount for its lightness. */
export function tintPaneBackground(bg: string, color: string): string {
  return mixHex(bg, color, isDarkColor(bg) ? PANE_TINT.dark : PANE_TINT.light);
}

/** `base` (the user's terminal theme) with its background tinted toward
 *  `machineId`'s color. Only the background changes: selection stays blue,
 *  every text color stays as tuned. Stable while its inputs are, so the
 *  terminal is not re-themed on every render. */
export function useMachineTerminalTheme(
  machineId: string,
  base: TerminalThemeConfig | undefined,
): TerminalThemeConfig {
  const color = useMachineColor(machineId);
  return useMemo(() => {
    const config = base ?? DEFAULT_THEME;
    const bg = config.theme.background ?? DEFAULT_THEME.theme.background!;
    return { ...config, theme: { ...config.theme, background: tintPaneBackground(bg, color) } };
  }, [base, color]);
}

/**
 * The layer over a remote terminal that says where it runs: one faint
 * diagonal hostname, anchored to the pane's right edge at mid-height, and the
 * user's background image (dimmed over a dark terminal) when they set one. Blended `lighten` over a dark terminal
 * (`multiply` over a light one), so glyphs brighter than the mark are never
 * dimmed — the mark reads as behind the text. Decorative: the hostname is
 * already in the tab's accessible name.
 */
export function MachineCanvas({
  machineId,
  hostname,
  background,
  color: colorOverride,
  className = "",
}: {
  machineId: string;
  /** The watermark's words — see {@link machineWatermarkText}. */
  hostname: string;
  /** The terminal's (tinted) background, to pick the blend. */
  background: string;
  /** A color being previewed, not yet the Machine's. */
  color?: string;
  className?: string;
}) {
  const savedColor = useMachineColor(machineId);
  const color = colorOverride ?? savedColor;
  const dark = isDarkColor(background);
  const { image, imageOpacity } = useMachineBackground(machineId);
  return (
    <div
      aria-hidden
      data-testid={`machine-canvas-${machineId}`}
      className={`pointer-events-none absolute inset-0 z-10 overflow-hidden [container-type:size] ${className}`}
      style={{ mixBlendMode: dark ? "lighten" : "multiply" }}
    >
      {image && (
        <img
          alt=""
          src={image}
          data-testid={`machine-canvas-image-${machineId}`}
          className="absolute inset-0 h-full w-full object-cover"
          style={{
            opacity: imageOpacity,
            filter: dark
              ? `brightness(${IMAGE_DIM.brightness}) saturate(${IMAGE_DIM.saturate})`
              : undefined,
          }}
        />
      )}
      <span
        data-testid={`machine-watermark-${machineId}`}
        className="absolute top-1/2 right-4 whitespace-nowrap font-sans leading-none select-none"
        style={{
          color,
          opacity: image ? WATERMARK_OPACITY.withImage : WATERMARK_OPACITY.alone,
          fontSize: "clamp(13px, 4cqw, 22px)",
          fontWeight: 600,
          transform: "translateY(-50%) rotate(-9deg)",
          transformOrigin: "right center",
        }}
      >
        {hostname}
      </span>
    </div>
  );
}
