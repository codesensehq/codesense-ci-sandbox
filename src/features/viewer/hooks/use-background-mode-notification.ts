// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { logger } from "@/shared/lib/logger";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { openAppSettings } from "@/features/settings/lib/open-settings";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { BACKGROUND_MODE_ENTERED_EVENT } from "@/tauri/events";

/** SW-AU-34 (AC-5): the one-time explainer copy — the app is still running and driving
 *  automations after a window close, and how to get it back / turn it off. Exported for
 *  the unit test. */
export const BACKGROUND_MODE_EXPLAINER =
  "codesense is still running in the background — your automations keep going. Click the dock icon to reopen it, or turn on Settings › Quit on window close.";

/** SW-AU-34 (AC-5): show the explainer only if the user hasn't seen it yet. Pure so the
 *  one-shot gate is unit-testable without a live event. `undefined` (fresh profile) ⇒
 *  show. */
export function shouldShowBackgroundExplainer(hasSeen: boolean | undefined): boolean {
  return !hasSeen;
}

/**
 * SW-AU-34 (AC-5): surface the one-time "still running in the background" explainer the
 * first time a window close is handled as background-on-close. Mounted once at app root
 * beside `useQuitOnCloseSetting`. Listens for {@link BACKGROUND_MODE_ENTERED_EVENT}
 * (emitted by the Rust close handler) and fires a single `toast.info`, then persists
 * `hasSeenBackgroundExplainer` so it never repeats. Passive: no polling, no synchronous
 * IPC — a background subscription that never blocks a frame. StrictMode-safe (cancel
 * flag + promise-teardown), as in the worktrees-panel listeners.
 */
export function useBackgroundModeNotification(): void {
  useEffect(() => {
    let cancelled = false;
    const unlisten = listen(BACKGROUND_MODE_ENTERED_EVENT, () => {
      if (cancelled) return;
      const store = useViewerStore.getState();
      if (!shouldShowBackgroundExplainer(store.settings.hasSeenBackgroundExplainer)) {
        return;
      }
      logger.debug("system", "background-mode explainer: first close");
      // Persist the one-shot flag before showing so a rapid second close can't race
      // a duplicate toast in (the listener reads the just-written state).
      store.updateSettings({ hasSeenBackgroundExplainer: true });
      toast.info(BACKGROUND_MODE_EXPLAINER, {
        action: {
          label: "Open settings",
          onClick: () => openAppSettings("general"),
        },
      });
    });
    return () => {
      cancelled = true;
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
}
