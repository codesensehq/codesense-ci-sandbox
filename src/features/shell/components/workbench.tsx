// SPDX-License-Identifier: MIT
import { FileNavSlotHost } from "../slots/file-nav-slot-host";
import { DockSlotHost } from "../slots/dock-slot-host";
import { createPortal } from "react-dom";
import {
  markSwitchRevealed,
  startSwitchProfile,
  useSwitchCommitMark,
} from "@/shared/lib/switch-profile";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  pointerWithin,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";

import { useTeamRenderGate } from "@/app/hooks/use-team-render-gate";
import { TeamWorkspaceEmptyState } from "@/features/dashboard/components/team-workspace-empty-state";
import {
  resolveMainTreeSurfaces,
  teamPaneVisible,
} from "@/features/home-base/lib/home-base-routing";
import { GlobalCli } from "@/features/viewer/components/global-cli";
import {
  RIGHT_PANE_RATIO_VAR,
  ScratchNoteWidthDivider,
} from "@/features/viewer/components/scratch-note-width-divider";
import { SharedWorkspaceTray } from "@/features/viewer/components/shared-workspace-tray";
import {
  TERM_SPLIT_VAR,
  TerminalSplitDivider,
} from "@/features/viewer/components/terminal-split-divider";
import {
  useTerminalSplitRatio,
  useTerminalVisible,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import {
  toggleRightPaneWithFocus,
  toggleTerminalWithFocus,
} from "@/features/viewer/lib/focus-cascade";
import { CliDragStaleContext } from "@/features/viewer/lib/cli-drag-scope";
import { registerRightPaneHost } from "@/features/viewer/lib/right-pane-host-registry";
import { parseTileZoneId, unwrapDragWorkspaceId } from "@/features/viewer/lib/terminal-drag";
import {
  isConsoleLayout,
  isTerminalColumnShown,
  isTerminalSplitShown,
} from "@/features/viewer/lib/terminal-column";
import { registerTerminalSection } from "@/features/viewer/lib/terminal-section-registry";
import { dropWorkspace, takeTrayReorderIntent } from "@/features/viewer/lib/tray-reorder-drag";
import { WorkspaceDragGhost } from "@/features/viewer/components/workspace-drag-ghost";
import { TeamWorkspaceView } from "@/features/viewer/views/team-workspace-view";
import { ErrorBoundary } from "@/shared/components/error-boundary";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { usePanelRegistry } from "../right-pane/panel-registry";
import { selectRenderableLayout } from "../right-pane/right-pane-layout";
import { isPanelOverlayShown } from "../right-pane/panel-maximize-boundary";

import { NoTeamTabHost } from "./no-team-tab-host";
import { TeamCrashFallback } from "./root-crash-fallback";

/** Stable empty list for the `openedTeams` selector, so a nullish slice never makes a new array
 *  (a new reference every render would re-render every subscriber). */
const EMPTY_TEAMS: readonly string[] = Object.freeze<string[]>([]);

/** SW-EW-17 / SW-EW-33: prefer a tile EDGE zone under the pointer, so a directional drop wins over
 *  the whole-grid droppable underneath. With tiles but no edge zone under the pointer (a tile's
 *  inert center, or the dragged tile's own), there is no valid drop: a workspace never drops INTO
 *  another, only splits at an edge, so the drag snaps back. The whole-grid droppable is the
 *  fallback only with no tile zones at all (an empty grid: the drop opens the first tile). */
const tileZonePriorityCollision: CollisionDetection = (args) => {
  const hits = pointerWithin(args);
  const zoneHits = hits.filter((h) => parseTileZoneId(String(h.id)));
  if (zoneHits.length > 0) return zoneHits;
  const tilesExist = args.droppableContainers.some((c) => parseTileZoneId(String(c.id)));
  return tilesExist ? [] : hits;
};

/** The workspace being dragged (for the ghost), and the team it was dragged in. */
interface ActiveDrag {
  workspaceId: string;
  teamKey: string;
}

/**
 * The window's main row, owned by the shell. Its left column stacks the team views (each opened
 * team keeps its nav and viewer, shown while it is the active team), the split divider, the
 * terminal section (the Agents pane and the one global CLI) and the one workspace tray; its right
 * column holds one right-pane host per team. One drag-and-drop context spans them, so a pill
 * dragged from the tray splits a tile and a tile dragged onto the tray's band reorders.
 *
 * The CLI, the tray, the divider and the terminal section always show the active team and never
 * remount on a team switch: only their content changes. Each team's right pane renders into its
 * own host, so its state and its launch context survive a switch away and back.
 *
 * Only the warm teams (the active one and the most recent others, pinned while one holds unsaved
 * edits) keep their view mounted, hidden while another team is active (RF-perf-10); the rest
 * unmount to reclaim renderer memory.
 */
export function Workbench() {
  // Debug builds only: time each team switch (store write, commit, effects, reveal) into
  // window.__csDebug.
  useEffect(() => startSwitchProfile(), []);
  const openedTeams = useViewerStore((s) => s.openedTeams ?? EMPTY_TEAMS);
  const activeOrgSlug = useOrgStore((s) => s.activeOrgSlug);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const activeTeamKey = useMemo(
    () => (activeOrgSlug !== null ? buildTeamKey(activeOrgSlug, activeTeamName) : null),
    [activeOrgSlug, activeTeamName],
  );
  useSwitchCommitMark(activeTeamKey);
  // SF-11: per-team error-boundary reset nonces. A team's crash fallback (Retry) bumps its nonce
  // to remount just that team's subtree; normal switches keep the boundary key stable.
  const [teamResetNonce, setTeamResetNonce] = useState<Record<string, number>>({});
  const [cliResetNonce, setCliResetNonce] = useState(0);
  const [teamRecoveryHost, setTeamRecoveryHost] = useState<HTMLDivElement | null>(null);
  // Closing a team drops its reset generation, even when nobody retries afterwards.
  useEffect(() => {
    setTeamResetNonce((m) => {
      const entries = Object.entries(m).filter(([k]) => openedTeams.includes(k));
      return entries.length === Object.keys(m).length ? m : Object.fromEntries(entries);
    });
  }, [openedTeams]);
  // A Retry also drops the nonces of teams no longer open, so the map stays bounded.
  const retryTeam = useCallback(
    (tk: string) =>
      setTeamResetNonce((m) => {
        const open = Object.fromEntries(Object.entries(m).filter(([k]) => openedTeams.includes(k)));
        return { ...open, [tk]: (m[tk] ?? 0) + 1 };
      }),
    [openedTeams],
  );

  // RF-perf-10: the render gate. A team with unsaved edits is pinned warm, since its live editor
  // buffer only exists in the mounted subtree.
  const editingDirtyPaths = useViewerStore((s) => s.editingDirtyPaths);
  const dirtyTeamKeys = useMemo(() => {
    const dirty = new Set<string>();
    for (const [tk, paths] of Object.entries(editingDirtyPaths ?? {})) {
      if ((paths?.length ?? 0) > 0) dirty.add(tk);
    }
    return dirty;
  }, [editingDirtyPaths]);
  const teamRenderSet = useTeamRenderGate({ openedTeams, dirtyTeamKeys });

  // The team-scoped empty-state ladder: no opened team, no active team, or an active team with no
  // mounted view (never a dead blank; its action opens the team).
  const { showTeamEmptyState } = resolveMainTreeSurfaces({
    openedTeamsCount: openedTeams.length,
    activeTeamName,
    activeTeamOpened: activeTeamKey !== null && openedTeams.includes(activeTeamKey),
  });
  // The team whose view (and right pane) is on screen: the active team, once it has a view.
  const shownTeam = showTeamEmptyState ? null : activeTeamKey;
  // The CLI follows the active team itself, not the shown one: it never collapses or unmounts
  // dock and tray around a view that is still being opened. Without any team there is none.
  const cliTeam = activeTeamName !== null ? activeTeamKey : null;

  // D39: the CLI's chrome is one setting for every team.
  const terminalVisible = useTerminalVisible();
  const terminalColumnShown = useViewerStore((s) => isTerminalColumnShown(s, cliTeam !== null));
  const terminalSplitShown = useViewerStore((s) => isTerminalSplitShown(s, cliTeam !== null));
  const terminalSplitRatio = useTerminalSplitRatio();
  // Full screen needs a view to give way to: with the active team not (yet) open, the empty-state
  // ladder stays reachable beside the CLI.
  const isConsole = useViewerStore((s) => isConsoleLayout(s, shownTeam !== null));

  // SW-LN-B37: the first run with no saved layout settles it once, from the first team the CLI
  // shows (a team with tiles opens the pane, one without starts collapsed). A saved layout stands.
  useEffect(() => {
    if (cliTeam) useViewerStore.getState().settleInitialLayout(cliTeam);
  }, [cliTeam]);

  // F-6 AC-10: the right pane's open state and width are global.
  const rightPaneOpen = useViewerStore((s) => s.rightPaneOpen);
  const rightPaneRatio = useViewerStore((s) => s.rightPaneRatio);
  const rightColumnShown = shownTeam !== null && rightPaneOpen;
  const panels = usePanelRegistry((s) => s.panels);
  const rightColumnDocked = useViewerStore((s) => {
    if (!shownTeam || !s.rightPaneOpen) return false;
    const maximized = s.maximizedPanel[shownTeam];
    if (!isPanelOverlayShown(s.rightPaneLayout, panels[maximized], !!maximized)) return true;
    const layout = selectRenderableLayout(s.rightPaneLayout, (id) => panels[id] != null);
    return layout.sections.some((section) => section.group.panelIds.some((id) => id !== maximized));
  });
  // The row the right-pane divider measures and writes its drag variable on; the left column the
  // split divider measures and writes TERM_SPLIT_VAR on (the viewer and the terminal read it).
  const mainRowRef = useRef<HTMLDivElement>(null);
  const leftColumnRef = useRef<HTMLDivElement>(null);
  // The one terminal section, registered so surfaces laid out against the split (the full-screen
  // inbox) find it and follow it.
  const registerSection = useCallback(
    (el: HTMLDivElement | null) => (el ? registerTerminalSection(el) : undefined),
    [],
  );

  // PT-13: restore-by-drag sensors. A short distance lets a click on a minimized pill still
  // reopen it (EW-11) while a deliberate drag starts one.
  const dragTargetRef = useRef<EventTarget | null>(null);
  const dndSensors = useSensors(
    useSensor(PointerSensor, {
      activationConstraint: { distance: 6 },
      onActivation: ({ event }) => {
        const target = event.target;
        dragTargetRef.current =
          target instanceof Node ? (target.ownerDocument ?? document) : document;
      },
    }),
  );
  const [activeDrag, setActiveDrag] = useState<ActiveDrag | null>(null);
  const cancelledDragRef = useRef(false);
  const endDrag = useCallback(() => {
    setActiveDrag(null);
    takeTrayReorderIntent();
  }, []);
  const handleDragStart = (event: DragStartEvent) => {
    if (event.active?.id == null || !cliTeam) return;
    cancelledDragRef.current = false;
    // SW-EW-33: unwrap the `tray:` namespace so the ghost names the bare workspace id.
    setActiveDrag({ workspaceId: unwrapDragWorkspaceId(event.active.id), teamKey: cliTeam });
  };
  const handleDragEnd = (event: DragEndEvent) => {
    const dragTeamKey = activeDrag?.teamKey ?? cliTeam;
    setActiveDrag(null);
    // A drag that outlived its team (the team changed mid-drag) drops nothing.
    if (cancelledDragRef.current || !dragTeamKey || dragTeamKey !== cliTeam) {
      takeTrayReorderIntent();
      return;
    }
    // A pill released in the tray's own band reorders the tray (and the Agents pane's cards); one
    // on a tile's edge splits or moves there, and one on a tile's center or the grid restores or
    // focuses it.
    dropWorkspace(dragTeamKey, event.active?.id, event.over?.id);
  };
  // Cancel only this pointer gesture on a team switch; a synthetic resize would notify every
  // window listener. The pointer sensor listens on the originating document, which survives
  // removal of the old team's draggable element.
  const dragTeam = activeDrag?.teamKey ?? null;
  const staleDrag = dragTeam !== null && dragTeam !== cliTeam;
  useEffect(() => {
    if (!staleDrag) return;
    cancelledDragRef.current = true;
    dragTargetRef.current?.dispatchEvent(new Event("pointercancel", { bubbles: true }));
    endDrag();
  }, [staleDrag, endDrag]);

  return (
    <DndContext
      sensors={dndSensors}
      collisionDetection={tileZonePriorityCollision}
      onDragStart={handleDragStart}
      onDragEnd={handleDragEnd}
      onDragCancel={endDrag}
    >
      <CliDragStaleContext value={staleDrag}>
        <div
          data-surface="canvas"
          ref={mainRowRef}
          data-testid="workbench"
          className="relative flex flex-1 min-h-0 overflow-hidden bg-background text-foreground"
        >
          <div ref={leftColumnRef} className="flex-1 flex flex-col overflow-hidden min-w-0">
            {/* The viewer region: the team views (nav and viewer) stacked, the active team's shown.
              Hidden in full screen, where the CLI fills the column; sized by the split while the
              CLI shows as a pane. */}
            <div
              data-testid="workbench-viewer-region"
              className="flex flex-col overflow-hidden min-h-0"
              style={
                isConsole
                  ? { display: "none" }
                  : {
                      flex:
                        cliTeam && terminalSplitShown
                          ? `var(${TERM_SPLIT_VAR}, ${terminalSplitRatio})`
                          : 1,
                    }
              }
            >
              {showTeamEmptyState && (
                <div className="flex-1 min-h-0 overflow-hidden">
                  {/* SW-FU-B7: management openers land here as tabs when there is no team
                    content area; the empty state shows beneath. */}
                  <NoTeamTabHost>
                    <TeamWorkspaceEmptyState />
                  </NoTeamTabHost>
                </div>
              )}
              <FileNavSlotHost />
              {openedTeams.map((tk) => (
                // The keyed wrapper stays for every opened team (stable reconciliation, hidden
                // while another team is active); its view renders only while the gate keeps it.
                <div
                  key={tk}
                  className="flex-1 min-h-0 overflow-hidden"
                  style={{
                    // Settings is a tab inside the team's view, so the view stays visible for it.
                    display: teamPaneVisible({
                      settingsPanelOpen: false,
                      isActiveTeam: activeTeamKey === tk,
                    })
                      ? undefined
                      : "none",
                  }}
                >
                  {teamRenderSet.has(tk) && (
                    // SF-11: a crash in one team's subtree (its portaled right pane included)
                    // degrades just this team; the shell, the CLI and the other teams stay alive.
                    <ErrorBoundary
                      key={`team-eb:${tk}:${teamResetNonce[tk] ?? 0}`}
                      scope={`team:${tk}`}
                      fallback={
                        <TeamCrashRecovery
                          teamKey={tk}
                          onRetry={() => retryTeam(tk)}
                          consoleMode={isConsole}
                          shown={tk === activeTeamKey}
                          host={teamRecoveryHost}
                        />
                      }
                    >
                      <TeamWorkspaceView teamKey={tk} />
                    </ErrorBoundary>
                  )}
                </div>
              ))}
            </div>

            {/* SW-FX-8: the split divider is drawn in the pane layout whether the CLI shows or is
              collapsed, so its border stays on screen as the click-to-open target. Full screen
              has no viewer to split from. */}
            {cliTeam && !isConsole && (
              <TerminalSplitDivider
                leftColumnRef={leftColumnRef}
                viewerRatio={terminalSplitRatio}
                collapsed={!terminalVisible}
                onToggle={() => toggleTerminalWithFocus(cliTeam)}
              />
            )}

            <ErrorBoundary
              key={`cli:${cliResetNonce}`}
              scope="global-cli"
              fallback={
                <TeamCrashFallback
                  teamKey="global-cli"
                  onRetry={() => setCliResetNonce((n) => n + 1)}
                />
              }
            >
              {/* The terminal section: the Agents pane on the left and the one CLI. Always mounted
              at this position; it takes the rest of the split (SF-7: the same TERM_SPLIT_VAR,
              inverted), the whole column in full screen, nothing while the CLI is collapsed. */}
              <div
                ref={registerSection}
                data-testid="terminal-section"
                className="flex overflow-hidden"
                style={
                  !cliTeam
                    ? { flex: "0 0 0px" }
                    : isConsole
                      ? { flex: 1 }
                      : {
                          flex: terminalSplitShown
                            ? `calc(1 - var(${TERM_SPLIT_VAR}, ${terminalSplitRatio}))`
                            : "0 0 0px",
                        }
                }
              >
                {cliTeam && <DockSlotHost teamKey={cliTeam} visible={terminalColumnShown} />}
                <GlobalCli teamKey={cliTeam} />
              </div>

              {/* The workspace tray: one, at the bottom of the whole left column, so it never leaves
              the screen (under the CLI when it shows, under the viewer when it is collapsed). In
              full screen it portals its top part into the title bar. A direct child of this
              column: the split divider measures the column above it. */}
              {cliTeam && <SharedWorkspaceTray teamKey={cliTeam} />}
            </ErrorBoundary>
          </div>

          <div
            ref={setTeamRecoveryHost}
            className="absolute inset-x-0 top-0 z-50 pointer-events-none"
          />

          {/* The right-pane divider: always drawn with a team on screen, open or closed, so the
            pane's left border stays as the click-to-open target. Its drag writes
            RIGHT_PANE_RATIO_VAR on this row. */}
          {shownTeam !== null && (
            <ScratchNoteWidthDivider
              containerRef={mainRowRef}
              collapsed={!rightPaneOpen}
              onToggle={() => toggleRightPaneWithFocus(shownTeam)}
            />
          )}

          {/* The right column: one host per opened team, the active team's shown. Each team view
            portals its right pane (Inbox, notes and custom panes) into its own host. */}
          <div
            data-testid="right-pane-column"
            data-surface="viewer"
            className="flex-shrink-0 overflow-hidden"
            style={{
              // Keep the fixed overlay visible while its empty docked slot takes no width.
              flex: rightColumnDocked
                ? `0 0 calc(var(${RIGHT_PANE_RATIO_VAR}, ${rightPaneRatio}) * 100%)`
                : "0 0 0px",
              display: rightColumnShown ? undefined : "none",
            }}
          >
            {openedTeams.map((tk) => (
              <RightPaneHost key={tk} teamKey={tk} shown={tk === activeTeamKey} />
            ))}
          </div>
        </div>
        {/* SW-EW-17: a ghost of the dragged workspace, so edge targeting has a clear payload. */}
        <DragOverlay dropAnimation={null}>
          {activeDrag && !staleDrag ? (
            <WorkspaceDragGhost teamKey={activeDrag.teamKey} workspaceId={activeDrag.workspaceId} />
          ) : null}
        </DragOverlay>
      </CliDragStaleContext>
    </DndContext>
  );
}

/** One team's right-pane host. Its element never changes for the team's lifetime, so the pane
 *  portaled into it keeps its state. */
function RightPaneHost({ teamKey, shown }: { teamKey: string; shown: boolean }) {
  const register = useCallback(
    (el: HTMLDivElement | null) => (el ? registerRightPaneHost(teamKey, el) : undefined),
    [teamKey],
  );
  useEffect(() => {
    if (shown) markSwitchRevealed();
  }, [shown]);
  return (
    <div
      ref={register}
      data-right-pane-host={teamKey}
      className="h-full"
      style={{ display: shown ? undefined : "none" }}
    />
  );
}

/** A team's portal errors still belong to that team. Console mode hides the viewer containing
 * its boundary, so recovery portals into a shell host that stays visible in either layout. */
function TeamCrashRecovery({
  teamKey,
  onRetry,
  consoleMode,
  shown,
  host,
}: {
  teamKey: string;
  onRetry: () => void;
  consoleMode: boolean;
  shown: boolean;
  host: HTMLElement | null;
}) {
  const recovery = <TeamCrashFallback teamKey={teamKey} onRetry={onRetry} />;
  if (!consoleMode) return recovery;
  return shown && host
    ? createPortal(<div className="pointer-events-auto">{recovery}</div>, host)
    : null;
}
