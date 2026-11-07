// SPDX-License-Identifier: MIT
import { runtimeBadgeTokens } from "./runtime-badges";
import codesense from "./codesense.tokens.json";
import dracula from "./dracula.tokens.json";
import github from "./github.tokens.json";
import nord from "./nord.tokens.json";
import catppuccin from "./catppuccin.tokens.json";
import confluence from "./confluence.tokens.json";
import { compileTheme, validateTheme, type ThemeMode } from "./compiler";

const bundled = <T extends { name: string }>(theme: T): T => ({
  ...theme,
  name: validateTheme(theme).name,
});
export const BUNDLED_THEMES = {
  codesense: bundled(codesense),
  dracula: bundled(dracula),
  github: bundled(github),
  nord: bundled(nord),
  catppuccin: bundled(catppuccin),
  confluence: bundled(confluence),
};
export type AppTheme = keyof typeof BUNDLED_THEMES;
export const DEFAULT_APP_THEME: AppTheme = "codesense";
export const isAppTheme = (id: unknown): id is AppTheme =>
  typeof id === "string" && id in BUNDLED_THEMES;
const cache = new Map<string, Record<string, string>>();
export function themeTokens(id: AppTheme, mode: ThemeMode) {
  const key = `${id}/${mode}`;
  let value = cache.get(key);
  if (!value) {
    const compiled = compileTheme(BUNDLED_THEMES[id], mode);
    value = { ...compiled, ...runtimeBadgeTokens(compiled) };
    cache.set(key, value);
  }
  return value;
}
