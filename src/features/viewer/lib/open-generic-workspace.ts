// SPDX-License-Identifier: MIT
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { getActiveTeamKey } from "@/shared/lib/team-key";

/**
 * SW-GW-1: the "Open workspace here" affordance's handler (the picker's Generic
 * Workspace entry). Prompts for a directory via the native folder picker — the
 * same `@tauri-apps/plugin-dialog` folder picker used by Add repo / Attach local
 * clone — then opens (or focuses) a plain CLI workspace rooted there.
 *
 * No provision/resolve step is needed: the chosen dir already exists, so it
 * just mints a per-dir workspace and opens it. A cancelled
 * picker (or a multi-select, which returns an array) is a quiet no-op. The open
 * itself is synchronous in the store, so the tile paints immediately.
 */
export async function openGenericWorkspace(): Promise<void> {
  const dir = await openFolderDialog({ directory: true, title: "Open workspace here" });
  // `directory: true` without `multiple` resolves to a single path string, or
  // `null` when the user cancels — bail on anything that isn't a path.
  if (typeof dir !== "string") return;
  useViewerStore.getState().openOrFocusGenericWorkspace(getActiveTeamKey(), dir);
}
