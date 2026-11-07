// SPDX-License-Identifier: MIT
import { switchContext } from "@/features/viewer/lib/context-switch";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

/** A file to open, already resolved inside `team`'s content root. */
export interface TeamFile {
  org: string;
  team: string;
  path: string;
}

export interface OpenFileInTeamOptions {
  /** A pinned tab instead of the preview tab. */
  newTab?: boolean;
  hash?: string;
  /** The host's own opener for a file in the team on screen (keeps its preview
   *  and reveal behaviour); used only when `file` is in the active team. */
  navigate?: (
    path: string,
    options?: { openInNewTab?: boolean; hash?: string; keepLayout?: boolean },
  ) => void;
  /** An open the user did not ask for (a page script's): it keeps the current team and full-screen CLI on
   *  (see the viewer store's `openFile`). */
  keepLayout?: boolean;
  /** How to make another team active: the app's context switch, so the switch is recorded for
   *  ⌘[ and the user is oriented. Defaults to switchContext. */
  switchTeam?: (teamKey: string) => void;
}

/**
 * Open `file` in the viewer. A file in the active team opens through the host's
 * `navigate` when given; a file in another team (or org) switches to that team
 * first, then opens there. Throws `unavailable:` when the team can't be made
 * active (not in the org list, or hidden).
 */
export function openFileInTeam(file: TeamFile, options: OpenFileInTeamOptions = {}): void {
  const { newTab = false, hash, navigate, keepLayout = false, switchTeam } = options;
  const org = useOrgStore.getState();
  if (org.activeOrgSlug === file.org && org.activeTeamName === file.team && navigate) {
    const navOpts = {
      ...(newTab ? { openInNewTab: true } : {}),
      ...(hash ? { hash } : {}),
      ...(keepLayout ? { keepLayout } : {}),
    };
    navigate(file.path, newTab || keepLayout || hash ? navOpts : undefined);
    return;
  }
  const teamKey = buildTeamKey(file.org, file.team);
  if (!keepLayout) {
    if (switchTeam) switchTeam(teamKey);
    else switchContext({ teamKey }, "nav");
  }
  const now = useOrgStore.getState();
  if (!keepLayout && (now.activeOrgSlug !== file.org || now.activeTeamName !== file.team)) {
    throw new Error(`unavailable: can't switch to team ${file.team} in ${file.org}`);
  }
  const viewer = useViewerStore.getState();
  viewer.openTeam(teamKey);
  const fileOptions =
    keepLayout || hash
      ? { ...(keepLayout ? { keepLayout } : {}), ...(hash ? { hash } : {}) }
      : undefined;
  if (newTab) viewer.openFile(teamKey, "", file.path, fileOptions);
  else {
    // A clicked link must show its preview. Keep the user's last terminal mode so the
    // console toggle still returns to it; scripts leave the global layout alone.
    if (!keepLayout && useViewerStore.getState().layoutMode === "console") {
      useViewerStore.setState({ layoutMode: "shared", terminalVisible: true });
    }
    if (fileOptions) viewer.openFilePreview(teamKey, "", file.path, fileOptions);
    else viewer.openFilePreview(teamKey, "", file.path);
  }
}
