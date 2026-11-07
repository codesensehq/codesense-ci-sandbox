// SPDX-License-Identifier: MIT
import { DEFAULT_APP_THEME, isAppTheme } from "./index";

/** Retired surface preferences are folded once, before defaults are merged. */
export function migrateThemeSettings(
  saved: Record<string, unknown>,
): Record<string, unknown> & { appTheme: import("./index").AppTheme } {
  const { viewerTheme, editorTheme, ...rest } = saved;
  const legacy =
    editorTheme && editorTheme !== "default"
      ? editorTheme
      : viewerTheme !== "dracula"
        ? viewerTheme
        : undefined;
  const candidate = legacy === "github-dark" ? "github" : legacy;
  return {
    ...rest,
    appTheme: isAppTheme(rest.appTheme)
      ? rest.appTheme
      : isAppTheme(candidate)
        ? candidate
        : DEFAULT_APP_THEME,
  };
}
