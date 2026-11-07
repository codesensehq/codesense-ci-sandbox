// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { logger } from "@/shared/lib/logger";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { setQuitOnClose } from "@/tauri/commands";

/**
 * SW-AU-34 (AC-5): keep the Rust window-close handler's quit-on-close gate in sync
 * with the `quitOnClose` setting.
 *
 * Fires once at launch (after settings hydrate) and
 * again whenever the setting changes, pushing the bool to Rust so the close path
 * applies it LIVE (no restart). The effect is a single fire-and-forget `invoke` (a
 * sub-ms in-memory atomic store on the Rust side), so it runs after paint and never
 * blocks a frame. An undefined/absent setting resolves to `false` — background-on-close,
 * the shipped default (AC-6).
 */
export function useQuitOnCloseSetting(): void {
  const setting = useViewerStore((s) => s.settings.quitOnClose);
  useEffect(() => {
    setQuitOnClose(setting ?? false).catch((e) =>
      logger.debug("system", `set quit-on-close failed: ${String(e)}`),
    );
  }, [setting]);
}
