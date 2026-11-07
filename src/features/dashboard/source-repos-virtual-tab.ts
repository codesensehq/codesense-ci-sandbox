// SPDX-License-Identifier: MIT
import { openTeamRequired } from "@/features/shell/manage-teams-virtual-tab";
import { hasActiveTeam, openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useUiStore } from "@/shared/stores/ui-store";

import { ManageReposTab } from "./components/manage-repos-tab";

/**
 * SW-UX (app-flow redesign) — a team's source-repo management opens as a virtual
 * viewer tab (`virtual://source-repos`) instead of a modal. It's the ONE surface for
 * add / connect-account / remove, operating on the active team; `renderViewer`
 * resolves the sentinel path via the virtual-tab registry.
 *
 * SW-OC-8: the tab is the ONE prominent home for the three ways to get code into a
 * team (add-from-URL, attach-a-local-clone, use-a-managed-workspace) — the add
 * section is first-class in the tab content, so `openSourceRepos("add")` and
 * `"list"` both just open/focus it; the `intent` is cleared on arrival.
 */
export const SOURCE_REPOS_VIRTUAL_PATH = "virtual://source-repos";

let registered = false;

/** Register the source-repo management surface under its virtual path. Idempotent. */
export function registerSourceReposTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(SOURCE_REPOS_VIRTUAL_PATH, "Source repos", ManageReposTab);
}

/**
 * Open (or focus) the source-repos tab in the active team's content area. Singleton:
 * re-opening focuses the existing tab (`openFile` de-dupes by path). SW-OC-8: the
 * add section is now always prominent in the tab, so `intent` no longer drives
 * layout — both `"add"` and `"list"` open/focus the same self-sufficient tab.
 * Closes settings so the tab is visible. Source repos belong to a team, so with no
 * team active this lands on the tab that gets the user one (SW-FU-B7).
 */
export function openSourceRepos(intent: "list" | "add" = "list"): void {
  registerSourceReposTab();
  if (!hasActiveTeam()) {
    openTeamRequired();
    return;
  }
  useUiStore.getState().setSourceReposIntent(intent);
  useUiStore.getState().closeSettings();
  openManagementTab(SOURCE_REPOS_VIRTUAL_PATH);
}
