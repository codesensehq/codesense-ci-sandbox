// SPDX-License-Identifier: MIT
import terminalPalettes from "@/themes/terminal-palettes.json";
import type { ITheme } from "@xterm/xterm";

export type TerminalThemeName =
  | "github-dark"
  | "dracula"
  | "nord"
  | "catppuccin"
  | "solarized-dark";

export interface TerminalThemeConfig {
  theme: ITheme;
  chromeBg: string;
}

export const TERMINAL_THEMES: Record<TerminalThemeName, TerminalThemeConfig> = terminalPalettes;

// Default theme for backwards compatibility
export const TERMINAL_THEME = TERMINAL_THEMES["github-dark"].theme;
