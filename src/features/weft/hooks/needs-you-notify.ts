// SPDX-License-Identifier: MIT
import { getCurrentWindow } from "@tauri-apps/api/window";

import { revealSlot } from "@/features/shell/slots/reveal-slot";

/** Raise the app window before revealing the person's Inbox surface.
 * Window operations are best effort; none may block the reveal. */
export async function focusAndRevealBoard(): Promise<void> {
  try {
    const win = getCurrentWindow();
    await win.unminimize().catch(() => {});
    await win.show().catch(() => {});
    await win.setFocus().catch(() => {});
  } catch {
    // No Tauri window (test / web) — still reveal the Inbox below.
  }
  revealSlot("right-pane", { surface: "inbox", intent: { focusComposer: false } });
}
