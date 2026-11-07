// SPDX-License-Identifier: MIT
import { useLayoutEffect, useRef } from "react";
import { useTheme } from "next-themes";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { DEFAULT_APP_THEME } from "./index";
import { applyTheme } from "./runtime";

/** One owner bridges preference/mode changes to CSS and the global theme topic. */
export function AppThemeProvider() {
  const id = useViewerStore((s) => s.settings?.appTheme) ?? DEFAULT_APP_THEME;
  const terminal = useViewerStore((s) => s.settings.terminalTheme);
  const previousTerminal = useRef(terminal);
  const { resolvedTheme } = useTheme();
  const mode = resolvedTheme === "dark" ? "dark" : "light";
  useLayoutEffect(() => {
    applyTheme(id, mode, document.documentElement, terminal);
    liveHub.publishTheme(id, mode, previousTerminal.current !== terminal);
    previousTerminal.current = terminal;
  }, [id, mode, terminal]);
  return null;
}
