// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { resolveBackgroundAgentLimit } from "@/shared/lib/background-agent-limit";
import { logger } from "@/shared/lib/logger";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { dashboardSetBackgroundLimit } from "@/tauri/commands";

/**
 * SW-DX-17d (AC-3): keep the trusted-side background-agent limit in sync with the
 * user setting.
 *
 * Mirrors `use-background-setup-cap`: fires once at launch (after settings
 * hydrate) and again whenever `backgroundAgentLimit` changes, pushing the
 * resolved number to Rust so the cap applies LIVE (no restart). The effect is a
 * single fire-and-forget `invoke` (a sub-ms in-memory atomic store on the Rust
 * side), so it runs after paint and never blocks a frame. Lowering the limit only
 * bounds FUTURE backgrounding; already-backgrounded agents keep their slots.
 */
export function useBackgroundAgentLimit(): void {
  const setting = useViewerStore((s) => s.settings.backgroundAgentLimit);
  useEffect(() => {
    dashboardSetBackgroundLimit(resolveBackgroundAgentLimit(setting)).catch((e) =>
      logger.debug("dashboard", `set background limit failed: ${String(e)}`),
    );
  }, [setting]);
}
