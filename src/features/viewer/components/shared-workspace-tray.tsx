// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { parseSyntheticRepoKey, workspaceReposForTeam } from "@/shared/lib/build-synthetic-repo";
import { computeTeamGlyphMap } from "@/shared/lib/team-key";
import { useIsLightViewerTheme } from "@/shared/lib/viewer-theme";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";

import { useIsConsole } from "@/shared/hooks/use-is-console";
import { useWorkspaceBarCallbacks } from "../hooks/use-workspace-bar-callbacks";
import { useTeamTree } from "../lib/team-surface-store";
import { AgentsDockToggle } from "./agents-dock-toggle";
import { CliBottomChrome } from "./cli-bottom-chrome";
import { WorkspaceTray } from "./workspace-tray";

/**
 * The window's one workspace tray, for the active team (`teamKey`), mounted once in the shell's
 * workbench at the bottom of the left column. In the CLI pane it carries the CLI's controls at
 * its right end; in full screen it portals its top part (the flat switcher, or the epic rail when
 * grouped) into the title bar's slot. Epic grouping reads the team's published pipeline tree.
 */
export function SharedWorkspaceTray({ teamKey }: { teamKey: string }) {
  const tree = useTeamTree(teamKey);
  // T.3 D-3: a glyph per source repo, only when the team has two or more. Memoized on the
  // team's repos so the pills get a stable reference.
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  const glyphMap = useMemo(() => {
    const repos = workspaceReposForTeam(workspaceRepos, teamKey);
    if (repos.length < 2) return undefined;
    const slugs: string[] = [];
    for (const r of repos) {
      const parsed = parseSyntheticRepoKey(r.repoPath);
      if (parsed) slugs.push(`${parsed.owner}/${parsed.repo}`);
    }
    return computeTeamGlyphMap(slugs);
  }, [workspaceRepos, teamKey]);
  const isLightTheme = useIsLightViewerTheme();
  const callbacks = useWorkspaceBarCallbacks(teamKey);
  const setWorkspacePaletteOpen = useUiStore((s) => s.setWorkspacePaletteOpen);
  const isConsole = useIsConsole();
  // SW-EW-34/38: the title bar's switcher slot, which the tray fills in full screen.
  const consoleSwitcherSlot = useUiStore((s) => s.consoleSwitcherSlot);

  return (
    <WorkspaceTray
      teamKey={teamKey}
      tree={tree}
      glyphMap={glyphMap}
      isLightTheme={isLightTheme}
      onNewWorkspace={() => setWorkspacePaletteOpen(true)}
      onStoryStatusChange={callbacks.onStoryStatusChange}
      onOpenStoryDoc={callbacks.onOpenStoryDoc}
      // SW-EW-35: in the pane the CLI's controls ride the tray's right end; in full screen they
      // live in the title bar's right cluster instead.
      trailing={isConsole ? undefined : <CliBottomChrome leadingDivider />}
      leading={<AgentsDockToggle teamKey={teamKey} />}
      topChrome={isConsole}
      consoleRailSlot={isConsole ? consoleSwitcherSlot : null}
    />
  );
}
