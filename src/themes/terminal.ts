// SPDX-License-Identifier: MIT
import {
  TERMINAL_THEMES,
  type TerminalThemeConfig,
} from "@/features/viewer/components/terminal-constants";
import { themeTokens, DEFAULT_APP_THEME, type AppTheme } from "./index";
import type { ThemeMode } from "./compiler";
import type { TerminalTheme } from "@/shared/stores/viewer-store";
export const ANSI_NAMES = [
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
const cache = new Map<string, TerminalThemeConfig>();
export function resolveTerminalTheme(
  choice: TerminalTheme,
  id: AppTheme = DEFAULT_APP_THEME,
  mode: ThemeMode = "light",
): TerminalThemeConfig {
  if (choice !== "match-app") return TERMINAL_THEMES[choice] ?? TERMINAL_THEMES["github-dark"];
  const key = `${id}/${mode}`;
  let config = cache.get(key);
  if (!config) {
    const t = themeTokens(id, mode),
      v = (p: string) => t[`--theme-terminal-${p}`];
    config = {
      chromeBg: t["--theme-surface-terminal-bg"],
      theme: {
        background: v("bg"),
        foreground: v("fg"),
        cursor: v("cursor"),
        selectionBackground: v("selection"),
        ...Object.fromEntries(ANSI_NAMES.map((name, i) => [name, v(`ansi-${i}`)])),
      },
    };
    cache.set(key, config);
  }
  return config;
}
