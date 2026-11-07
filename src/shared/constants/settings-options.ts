// SPDX-License-Identifier: MIT
import type { TerminalTheme, StarshipPreset } from "@/shared/stores/viewer-store";

/**
 * Option constants for settings pickers. Single source of truth — do not
 * duplicate these lists. Used by the app-wide Settings panel rows.
 */

export const TERMINAL_THEME_OPTIONS: { value: TerminalTheme; label: string }[] = [
  { value: "match-app", label: "Match app theme" },
  { value: "github-dark", label: "GitHub Dark" },
  { value: "dracula", label: "Dracula" },
  { value: "nord", label: "Nord" },
  { value: "catppuccin", label: "Catppuccin" },
  { value: "solarized-dark", label: "Solarized Dark" },
];

// SW-FX-64: the `0` sentinel is "Maximum", NOT unbounded — it resolves to
// `MAX_SCROLLBACK` (a hard ceiling) so a single terminal can't grow its buffer
// without limit over a long session.
export const SCROLLBACK_OPTIONS: { value: number; label: string }[] = [
  { value: 1000, label: "1,000" },
  { value: 5000, label: "5,000" },
  { value: 10000, label: "10,000" },
  { value: 50000, label: "50,000" },
  { value: 0, label: "Maximum" },
];

export const STARSHIP_PRESET_OPTIONS: {
  value: StarshipPreset;
  label: string;
  description: string;
}[] = [
  { value: "compact", label: "Compact", description: "Directory + git + prompt on one line" },
  { value: "full", label: "Full", description: "Directory + git + node/rust + duration" },
  { value: "minimal", label: "Minimal", description: "Just prompt character + directory" },
  { value: "twoline", label: "Two-line", description: "Info on line 1, prompt on line 2" },
  { value: "plain", label: "Plain", description: "Your own shell prompt (no Starship)" },
];
