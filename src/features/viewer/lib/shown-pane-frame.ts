// SPDX-License-Identifier: MIT
import { queryShown } from "@/shared/lib/warm-scopes";

/** The on-screen tile frame for `workspaceId`, or null when it isn't shown. */
export function shownPaneFrame(workspaceId: string): HTMLElement | null {
  if (!workspaceId || typeof document === "undefined") return null;
  return queryShown(`[data-pane-id="${CSS.escape(workspaceId)}"]`);
}
