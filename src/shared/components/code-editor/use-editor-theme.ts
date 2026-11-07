// SPDX-License-Identifier: MIT
import { useTheme } from "next-themes";

import { resolveAppTheme } from "./themes/editor-theme";

export interface ResolvedEditorThemeView {
  /** Effective light/dark for CM's `dark` hint + inline `colorScheme` (RD-6). */
  theme: "light" | "dark";
}

/** Keep the editor and diff chrome on the same effective app mode. */
export function useEditorTheme(themeProp: "light" | "dark" | "auto"): ResolvedEditorThemeView {
  const { resolvedTheme } = useTheme();
  const appTheme = resolveAppTheme(themeProp, resolvedTheme);
  return { theme: appTheme };
}
