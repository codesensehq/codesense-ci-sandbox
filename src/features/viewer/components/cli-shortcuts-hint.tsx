// SPDX-License-Identifier: MIT
import { Keyboard, X } from "lucide-react";

import { openAppSettings } from "@/features/settings/lib/open-settings";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * A one-time, dismissible hint in the CLI empty state that points a new user at
 * the keyboard-navigation shortcuts (listed in Settings → Shortcuts). The "seen"
 * flag is a persisted global scalar, so the hint shows once and stays dismissed
 * across restarts (mirrors the workspace-tray coachmark). Following the link or
 * dismissing both mark it seen.
 */
export function CliShortcutsHint() {
  const seen = useViewerStore((s) => s.cliShortcutsHintSeen);
  const markSeen = useViewerStore((s) => s.markCliShortcutsHintSeen);
  if (seen) return null;

  const openShortcuts = () => {
    // Mark seen first so a failure opening Settings can't leave the hint to
    // reappear on every empty-state render.
    markSeen();
    openAppSettings("shortcuts");
  };

  return (
    <div
      data-testid="cli-shortcuts-hint"
      className="flex items-center gap-2 rounded border border-border bg-popover px-2 py-1 text-xs text-muted-foreground"
    >
      <Keyboard className="h-3.5 w-3.5" aria-hidden="true" />
      <span>Navigate by keyboard —</span>
      <button
        type="button"
        data-testid="cli-shortcuts-hint-open"
        aria-label="View keyboard shortcuts in Settings"
        className="underline underline-offset-2 hover:text-foreground"
        onClick={openShortcuts}
      >
        view shortcuts
      </button>
      <button
        type="button"
        aria-label="Dismiss"
        data-testid="cli-shortcuts-hint-dismiss"
        className="ml-1 text-muted-foreground hover:text-foreground"
        onClick={markSeen}
      >
        <X className="h-3 w-3" aria-hidden="true" />
      </button>
    </div>
  );
}
