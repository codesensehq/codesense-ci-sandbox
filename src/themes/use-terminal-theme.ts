// SPDX-License-Identifier: MIT
import { useTheme } from "next-themes";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { DEFAULT_APP_THEME } from "./index";
import { resolveTerminalTheme } from "./terminal";
export function useTerminalThemeConfig() {
  const choice = useViewerStore((s) => s.settings.terminalTheme);
  const id = useViewerStore((s) => s.settings.appTheme) ?? DEFAULT_APP_THEME;
  const { resolvedTheme } = useTheme();
  return resolveTerminalTheme(choice, id, resolvedTheme === "dark" ? "dark" : "light");
}
