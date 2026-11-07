// SPDX-License-Identifier: MIT
import { useTerminalThemeConfig } from "@/themes/use-terminal-theme";
import { useCallback, useMemo } from "react";

import { readEpicRepos, setEpicRepos } from "@/tauri/commands";
import { useInstanceCount } from "@/shared/hooks/use-instance-count";
import { useTeamContentContext } from "@/shared/hooks/use-team-content-context";
import { useTeamWorkspaces } from "@/shared/hooks/use-team-workspaces";
import {
  buildSyntheticRepoPathFromPrompt,
  parseSyntheticRepoKey,
  workspaceReposForTeam,
} from "@/shared/lib/build-synthetic-repo";
import { parseTeamKey } from "@/shared/lib/team-key";
import { OwningScopeProvider, useMountedScopes } from "@/shared/lib/warm-scopes";
import { isEpicShellWorkspace } from "@/shared/lib/workspace-predicates";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { startAgentInTeam } from "@/features/weft/lib/open-fresh-agent";

import {
  useActiveForegroundTileCount,
  useActiveWorkspaceId,
  useOpeningWorkspaceId,
  useSoftClosedForTeam,
} from "../lib/cli-workspace/store/terminal-selectors";
import { CliDragStaleContext, useCliDragStale } from "../lib/cli-drag-scope";
import { isTerminalColumnShown } from "../lib/terminal-column";
import { resolveEpicCommandContext } from "../lib/epic-workspace-opener";
import { buildEpicDocPathIndex } from "../lib/group-workspaces-by-epic";
import { resolveEpicDocPath as resolveEpicDocPathFor } from "../lib/resolve-epic-doc-path";
import { syncEpicWorktreesAfterRepoChange } from "../lib/sync-epic-worktrees-after-repo-change";
import { useTeamTree } from "../lib/team-surface-store";
import { useEpicRepoOverlay } from "../lib/use-epic-repo-overlay";
import { useWorkspaceBarCallbacks } from "../hooks/use-workspace-bar-callbacks";
import { useWorkspaceCycleIntercept } from "../hooks/use-workspace-cycle-intercept";
import { AgentRunOutputHost } from "./agent-run-output-host";
import { EpicRepoOverlay } from "./epic-repo-overlay";
import { TerminalEmptyState } from "./terminal-empty-state";
import { WorkspaceInitOverlay, WorkspaceTerminalPanels } from "./workspace-terminal-panels";

/** The teams whose grid the CLI keeps mounted: the active one plus the other warm ones, in the
 *  order the teams were opened so a switch never reorders (and so never remounts) a grid. */
function useCliTeams(activeTeam: string | null): string[] {
  const warm = useMountedScopes(useViewerStore((s) => s.openedTeams));
  return useMemo(
    () => (activeTeam === null ? [] : warm.includes(activeTeam) ? warm : [...warm, activeTeam]),
    [warm, activeTeam],
  );
}

/** The synthetic repo key rooting a team's terminals: its focused workspace's own repo (a
 *  slug-less epic workspace has none). */
function useCliRepoPath(teamKey: string | null): string {
  const activeWorkspaceId = useActiveWorkspaceId(teamKey ?? "");
  const sourceRepoSlug = useViewerStore((s) =>
    teamKey
      ? s.workspaces[teamKey]?.find((w) => w.id === activeWorkspaceId)?.sourceRepoSlug
      : undefined,
  );
  return useMemo(
    () =>
      teamKey && sourceRepoSlug
        ? (buildSyntheticRepoPathFromPrompt({ teamKey, sourceRepoSlug }) ?? "")
        : "",
    [teamKey, sourceRepoSlug],
  );
}

/**
 * The one CLI of the window, mounted once in the shell's workbench. It always shows the active
 * team (`teamKey`; null with no active team, when it shows nothing): its tiles, or its empty
 * state. This host stays mounted across team switches, and so do the grids of the warm teams:
 * each sits in its own wrapper that is only hidden while another team is active (the wrapper
 * never moves), so a switch between warm teams attaches, detaches and re-measures no terminal.
 * A team that falls out of the warm set unmounts its grid; its displays are parked in the
 * terminal manager with their PTYs running.
 */
export function GlobalCli({ teamKey }: { teamKey: string | null }) {
  useInstanceCount("cli");
  const repoPath = useCliRepoPath(teamKey);
  const teams = useCliTeams(teamKey);
  // AC-20: capture-phase intercept of Cmd+Opt+] / Cmd+Opt+[ inside the CLI, so xterm.js doesn't
  // read them as escape sequences. One CLI, so one listener: each key runs once.
  useWorkspaceCycleIntercept(teamKey ? repoPath : null);

  return (
    <div
      // CLI_HOST_SELECTOR: the one host the CLI's focus checks and terminal scans are scoped to.
      data-xterm-host
      data-repo-path={repoPath}
      data-testid="global-cli"
      className="flex flex-col h-full flex-1 min-w-0"
    >
      {/* The tiles, or the empty state, with the overlays that cover them (the epic repos, the
          opening workspace, an opened run's output) layered on top so the PTYs underneath
          survive. */}
      {teams.map((tk) => (
        <CliTeam key={tk} teamKey={tk} active={tk === teamKey} />
      ))}
    </div>
  );
}

function CliTeam({ teamKey, active }: { teamKey: string; active: boolean }) {
  const repoPath = useCliRepoPath(teamKey);
  const outerStale = useCliDragStale();
  return (
    // A hidden grid is never a drop target (its ids may repeat the shown team's).
    <CliDragStaleContext value={outerStale || !active}>
      <div
        data-cli-team={teamKey}
        className="relative flex flex-col flex-1 min-h-0"
        style={active ? undefined : { display: "none" }}
      >
        <OwningScopeProvider scope={teamKey}>
          <GlobalCliBody teamKey={teamKey} repoPath={repoPath} active={active} />
        </OwningScopeProvider>
      </div>
    </CliDragStaleContext>
  );
}

function GlobalCliBody({
  teamKey,
  repoPath,
  active,
}: {
  teamKey: string;
  repoPath: string;
  /** The team is the one on screen: a warm team's grid stays mounted but is not shown. */
  active: boolean;
}) {
  const workspaces = useTeamWorkspaces(teamKey);
  const activeWorkspaceId = useActiveWorkspaceId(teamKey);
  const activeWorkspaceObj = useMemo(
    () => workspaces.find((w) => w.id === activeWorkspaceId) ?? null,
    [workspaces, activeWorkspaceId],
  );
  const terminalShown = useViewerStore(isTerminalColumnShown) && active;
  const terminalThemeName = useTerminalThemeConfig();
  const teamContent = useTeamContentContext(teamKey);
  const workspaceBarCallbacks = useWorkspaceBarCallbacks(teamKey);
  const setWorkspacePaletteOpen = useUiStore((s) => s.setWorkspacePaletteOpen);
  // SW-SO-1: a story open whose record has not landed yet (null once it has).
  const openingWorkspaceId = useOpeningWorkspaceId(teamKey);
  // The team's minimized (closed but still running) workspaces: an empty CLI says how many
  // still run there.
  const softClosedForTeam = useSoftClosedForTeam(teamKey);
  const backgroundCount = Object.keys(softClosedForTeam ?? {}).length;
  // SW-EW-18 AC-8 / SW-EW-40: no foreground tiles in the active slot (a selected epic with no
  // open member included) shows a real empty state, never a spawnable bare terminal.
  const isEmptyTerminal = useActiveForegroundTileCount(teamKey) === 0;

  // SW-EW-9 / SW-EW-15: the epic repos overlay over an epic workspace's terminal. The add picker
  // draws from the team's attached source repos; "needs clone" from the config's local paths.
  const orgs = useOrgStore((s) => s.orgs);
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  const teamSourceRepos = useMemo(() => {
    const { orgSlug, teamName } = parseTeamKey(teamKey);
    const team = orgs.find((o) => o.slug === orgSlug)?.teams.find((t) => t.name === teamName);
    return (team?.source_repos ?? []).map((s) => `${s.owner}/${s.repo}`);
  }, [orgs, teamKey]);
  const clonedRepos = useMemo(() => {
    const set = new Set<string>();
    for (const r of workspaceReposForTeam(workspaceRepos, teamKey)) {
      const parsed = parseSyntheticRepoKey(r.repoPath);
      if (parsed && r.localPath) set.add(`${parsed.owner}/${parsed.repo}`);
    }
    return set;
  }, [workspaceRepos, teamKey]);
  // The team's pipeline tree, as its team view published it: no second scan.
  const tree = useTeamTree(teamKey);
  const epicDocPathIndex = useMemo(() => buildEpicDocPathIndex(tree), [tree]);
  // Resolve an epic's doc path, record-first (SW-EW-15): a slug-less epic workspace's tree never
  // scans, so the record threaded at open time is the truth. The focused workspace comes first,
  // so two epics sharing a leaf dir name resolve to the one the CLI shows.
  const resolveEpicDocPath = useCallback(
    (epicDirPath: string) => {
      const records = useViewerStore.getState().workspaces[teamKey] ?? [];
      const ordered = activeWorkspaceObj ? [activeWorkspaceObj, ...records] : records;
      return resolveEpicDocPathFor(ordered, epicDocPathIndex, epicDirPath);
    },
    [epicDocPathIndex, teamKey, activeWorkspaceObj],
  );
  const handleReadEpicRepos = useCallback(
    async (epicDirPath: string) => readEpicRepos(resolveEpicDocPath(epicDirPath)),
    [resolveEpicDocPath],
  );
  const handleSetEpicRepos = useCallback(
    async (epicDirPath: string, repos: string[], opts: { assemble: boolean }) => {
      const docPath = resolveEpicDocPath(epicDirPath);
      // Write the source of truth; the returned list re-renders the card at once.
      const next = await setEpicRepos(docPath, repos);
      // Fire-and-forget: sync the worktrees to the new repo set (an add assembles, a remove
      // reconciles). The write above already succeeded whatever the worktrees do.
      const { orgSlug, teamName } = parseTeamKey(teamKey);
      const ctx = resolveEpicCommandContext({ orgSlug, team: teamName ?? "_none" });
      syncEpicWorktreesAfterRepoChange(ctx, docPath, opts);
      return next;
    },
    [resolveEpicDocPath, teamKey],
  );
  const presentWorkspaceIds = useMemo(() => workspaces.map((w) => w.id), [workspaces]);
  // A warm team's hidden grid reads and draws no overlay: it is not on screen.
  const epicRepoOverlay = useEpicRepoOverlay(
    active ? activeWorkspaceObj : null,
    presentWorkspaceIds,
    handleReadEpicRepos,
    teamKey,
  );

  const canStartAgent =
    !!activeWorkspaceObj ||
    (activeWorkspaceId !== undefined && !!softClosedForTeam?.[activeWorkspaceId]) ||
    teamContent.kind === "ready";
  const startAgentUnavailable =
    teamContent.kind === "loading"
      ? "Loading team content…"
      : "Team content is unavailable. Open or refresh this team to start an agent.";

  return (
    <>
      {isEmptyTerminal ? (
        <TerminalEmptyState
          onOpenWorkspace={() => setWorkspacePaletteOpen(true)}
          onStartAgent={() =>
            startAgentInTeam(
              teamKey,
              teamContent.kind === "ready" ? teamContent.source.teamContentPath : "",
            )
          }
          startAgentUnavailable={canStartAgent ? undefined : startAgentUnavailable}
          backgroundCount={backgroundCount}
        />
      ) : (
        <WorkspaceTerminalPanels
          teamKey={teamKey}
          shown={terminalShown}
          teamHidden={!active}
          repoPath={repoPath}
          onOpenStoryDoc={workspaceBarCallbacks.onOpenStoryDoc}
          onStoryStatusChange={workspaceBarCallbacks.onStoryStatusChange}
        />
      )}
      {active &&
        isEpicShellWorkspace(activeWorkspaceObj) &&
        activeWorkspaceObj?.epicDirPath &&
        epicRepoOverlay.isOpen(activeWorkspaceObj.id) && (
          <EpicRepoOverlay
            key={`${teamKey}\0${activeWorkspaceObj.id}`}
            epicDirPath={activeWorkspaceObj.epicDirPath}
            teamSourceRepos={teamSourceRepos}
            clonedRepos={clonedRepos}
            onReadEpicRepos={handleReadEpicRepos}
            onSetEpicRepos={handleSetEpicRepos}
            onClose={() => epicRepoOverlay.close(activeWorkspaceObj.id)}
          />
        )}
      {/* SW-SO-1 / SW-FX-69: the pre-record "opening" overlay covers only an empty CLI, so live
          tiles stay visible while an additive open runs; it hands off to the in-tile overlay
          the moment the workspace record lands. Cancel aborts the open. */}
      {openingWorkspaceId && isEmptyTerminal && (
        <WorkspaceInitOverlay
          kind="story"
          identity={openingWorkspaceId}
          setupLabel="Checking worktree…"
          announce="Setting up worktree"
          themeConfig={terminalThemeName}
          onCancel={() =>
            useViewerStore.getState().cancelWorkspaceOpen(teamKey, openingWorkspaceId)
          }
        />
      )}
      {/* An opened run's output covers the tiles instead of replacing them, so the live
          terminals underneath keep their PTYs and come back unchanged on close. */}
      <AgentRunOutputHost teamKey={teamKey} repoPath={repoPath} visible={terminalShown} />
    </>
  );
}
