// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { logger } from "@/shared/lib/logger";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { setTerminalServerMenuBar } from "@/tauri/commands";

/**
 * App-wide macOS settings integration, outside the CLI workspace capability.
 * SV-8: keep the macOS menu-bar item for the terminal server in step with the
 * `terminalServerMenuBar` setting. Pushes once at launch (after settings
 * hydrate) and again whenever the setting changes, so turning it off removes
 * the item at once. An undefined setting resolves to `false` (off by default).
 */
export function useTerminalServerMenuBarSetting(): void {
  const setting = useViewerStore((s) => s.settings.terminalServerMenuBar);
  useEffect(() => {
    setTerminalServerMenuBar(setting ?? false).catch((e) =>
      logger.debug("system", `set terminal-server menu bar failed: ${String(e)}`),
    );
  }, [setting]);
}
