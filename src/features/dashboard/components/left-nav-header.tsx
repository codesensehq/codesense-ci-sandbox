// SPDX-License-Identifier: MIT
import { useScratchTeamUseState } from "@/shared/stores/scratch-team-use-store";
import { Lock } from "lucide-react";
import type { ResolvedNavSource } from "@/shared/hooks/use-nav-source";
import { useMemo } from "react";

import { AgentContextPivot } from "@/features/agent-context/components/agent-context-pivot";
import type { AgentContextPivot as PivotState } from "@/features/agent-context/hooks/use-agent-context";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";
import {
  GitStatusButton,
  PrivateRepoStatusButton,
} from "@/features/git-sync/components/git-status-button";
import { UnsavedTeamEditsButton } from "@/features/team-edits/components/unsaved-team-edits-button";
import { joinOrgPath } from "@/features/git-sync/lib/group-changes-by-node";
import { scopeChanges } from "@/features/git-sync/lib/nav-changes";
import { useContextGitSource } from "@/features/git-sync/lib/use-context-git-source";
import { useContextGit } from "@/features/git-sync/store/use-context-git-store";
import { useTeamRepoGlyphMap } from "@/shared/lib/pipeline-graph";
import { NO_TEAM_SENTINEL, useActiveTeamKey } from "@/shared/lib/team-key";
import { useIsLightViewerTheme } from "@/shared/lib/viewer-theme";
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * The left nav's scope row: whose view of the team the tree shows on the left, the context
 * repo's status button on the right (`⎇ branch | ↑N ↓M`).
 *
 * The org/team identity and its switcher live only in the title bar, so this row never restates
 * them. A two-column grid and an `@container`: the scope label sheds its qualifiers below 18rem,
 * the status button sheds its words, and an agent's context summary spans the line below both.
 * The row has no divider of its own: it reads as one block with the filter row underneath.
 *
 * It also mounts the one reader of the team's context repo (changed files + status) that the
 * button, the rows' tints, the Changes view and the tabs' diff strips all read.
 */
export function LeftNavHeader({
  teamKey,
  navRepoPath,
  pivot,
  agentContext,
  resolvedSource,
}: {
  resolvedSource?: ResolvedNavSource;
  teamKey: string;
  navRepoPath: string;
  pivot: PivotState;
  /** The selected agent's context, when the scope is "Context for ‹Agent›". */
  agentContext?: NavAgentContext;
}) {
  const [org, team] = teamKey.split(":");
  const scratchOn = useScratchTeamUseState(org, team);
  const { refreshStatus } = useContextGitSource(teamKey, navRepoPath, true, resolvedSource);
  const { status, changes, orgRoot, cloneRoot, error } = useContextGit(teamKey);
  const inScope = useMemo(
    () =>
      scopeChanges(changes, agentContext, status?.worktree ? cloneRoot : null).inScope.filter(
        (c) => c.kind !== "conflict",
      ),
    [changes, agentContext, status?.worktree, cloneRoot],
  );
  const agentNames = useMemo(() => pivot.agents.map((a) => a.name), [pivot.agents]);

  const openChanges = () => useViewerStore.getState().setNavLens(teamKey, { view: "changes" });
  const compare = (relPath: string) => {
    if (!orgRoot) return;
    const v = useViewerStore.getState();
    v.setNavLens(teamKey, { view: "changes" });
    v.openChangeTab(teamKey, navRepoPath, joinOrgPath(orgRoot, relPath), true);
  };

  return (
    <div
      className="@container grid flex-shrink-0 select-none grid-cols-[minmax(0,1fr)_auto] items-center gap-x-2 px-2 pt-1.5 pb-1"
      data-testid="left-nav-scope-row"
    >
      {resolvedSource?.source.kind === "private" ? (
        <span
          className="flex items-center gap-1 text-xs"
          title="Your private files. Only you see them. Choose main · your clone in the View menu to see the team files."
        >
          <Lock size={12} aria-hidden />
          Private <span className="text-muted-foreground @max-[18rem]:hidden">· only you</span>
        </span>
      ) : (
        <ScopePicker pivot={pivot} />
      )}
      <div className="col-start-2 row-start-1 flex min-w-0 items-center gap-1.5">
        {teamKey !== NO_TEAM_SENTINEL && <UnsavedTeamEditsButton teamKey={teamKey} />}
        {resolvedSource?.source.kind === "private" ? (
          <PrivateRepoStatusButton
            orgName={org}
            lastCommitAt={status?.lastCommitAt}
            uncommitted={inScope.length}
            error={error}
            onOpenChanges={openChanges}
            onRetry={refreshStatus}
          />
        ) : (
          status &&
          orgRoot && (
            <GitStatusButton
              teamKey={teamKey}
              // Commits aren't scoped to an agent's context, so its count is its files.
              status={agentContext && !status.worktree ? { ...status, ahead: 0 } : status}
              uncommitted={inScope.length}
              orgRoot={orgRoot}
              onOpenChanges={openChanges}
              onCompare={compare}
              onSettled={refreshStatus}
              agentNames={agentNames}
            />
          )
        )}
      </div>
      {resolvedSource?.source.kind === "private" && scratchOn === false && (
        <p className="col-span-2 pt-1 text-xs text-muted-foreground">
          Private items are off for {team}, so they don't apply in its work.
        </p>
      )}
    </div>
  );
}

/** Whose view of the team the tree below shows. Hidden only when there is no active team. */
function ScopePicker({ pivot }: { pivot: PivotState }) {
  const teamKey = useActiveTeamKey();
  const glyphMap = useTeamRepoGlyphMap(teamKey);
  const isLightTheme = useIsLightViewerTheme();
  if (teamKey === NO_TEAM_SENTINEL) return null;
  return (
    <div className="contents" data-testid="pipeline-nav-projection-header">
      <AgentContextPivot pivot={pivot} glyphMap={glyphMap} isLightTheme={isLightTheme} />
    </div>
  );
}
