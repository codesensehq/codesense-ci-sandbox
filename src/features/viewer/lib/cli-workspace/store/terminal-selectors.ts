// SPDX-License-Identifier: MIT
/**
 * The CLI workspace module's single door to the viewer store for REACTIVE
 * subscriptions — the render-time counterpart to `viewer-access.ts`'s imperative
 * `getViewerState()`. Terminal presentation subscribes to CLI state through these
 * hooks instead of importing the global `useViewerStore` singleton, so the seam
 * depends on this adapter rather than on the store shape (NFR6).
 *
 * Behaviour-preserving: each hook wraps the exact selector its call site used, so
 * subscription identity, granularity, and equality are unchanged.
 *
 * Atomic by design: one value per hook keeps React's `Object.is` bail-out intact.
 * A hook whose selector builds a NEW object/array each read (e.g. the whole
 * foreground slot or tile list) must wrap `useShallow`; a hook returning a
 * primitive or a stored reference needs nothing. Do not combine slices into one
 * object-returning hook — that returns a fresh object every tick and re-renders
 * on every unrelated store change. Hooks compose the store's pure slot selectors
 * verbatim; they never re-derive rail/slot scoping here.
 */

import { useShallow } from "zustand/react/shallow";
import {
  useViewerStore,
  activeForegroundLayout,
  activeForegroundTiles,
  resolveOpeningWorkspaceId,
  selectActiveTerminalTab,
  selectHeldTerminalOpens,
  selectMinimizedTerminalTabIndex,
  selectTerminalTabIndex,
  selectRunningTerminalTabIndex,
  selectWorkspaceTabs,
  workspacePanedTabIds,
  shouldRenderTabGrid,
  workspaceTabGridPreset,
  expandedWorkspaceId,
  selectExpandedWorkspaceId,
  expandedGridWorkspaceId,
  findTabLocationBySessionId,
  selectWorkspaceKey,
  teamTileOrder,
  type LayoutMode,
  type LayoutPreset,
  type TerminalTab,
  type TerminalTabIndex,
  type Workspace,
} from "@/shared/stores/viewer-store";

// SW-TP CAP-6: re-export the Workspace type through the seam so caller
// components in the CLI tree can describe their props without reaching
// through the arch-boundary into the global store module. The runtime
// value lives in the store; this is a type-only pass-through.
export type { Workspace };
import type { TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import { activeWorkspaceIdForTeam, teamLabelAmong, teamKeyHasTeam } from "@/shared/lib/team-key";

import { isTerminalColumnShown } from "../../terminal-column";

/** The shown-team gate for CLI controls, through the reactive store boundary. */
export function useTerminalColumnShown(teamKey: string): boolean {
  return useViewerStore((s) =>
    isTerminalColumnShown(s, teamKeyHasTeam(teamKey) && s.openedTeams.includes(teamKey)),
  );
}

/** Whether the terminal hosts' layout is known yet (the first snapshot
 *  landed): until then a workspace with no tabs is "not known", not empty. */
export function useTerminalLayoutHydrated(): boolean {
  return useViewerStore((s) => s.terminalLayout.hydrated);
}

/** Whether the terminal hosts have not answered for their layout yet. */
export function useTerminalHostsUnreachable(): boolean {
  return useViewerStore((s) => s.terminalHostsUnreachable);
}

/** Whether This Mac's terminal server is admitted but unreachable: the app is
 *  reconnecting to it, and the terminals it runs keep running. */
export function useLocalTerminalServerReconnecting(): boolean {
  return useViewerStore((s) =>
    Object.values(s.terminalLayout.admission).some(
      (row) => row.member.machineId === "local" && row.state === "unavailable",
    ),
  );
}

/** How many of the workspace's new terminals wait for a terminal-server restart. */
export function useHeldTerminalOpens(teamKey: string, workspaceId: string): number {
  return useViewerStore((s) => selectHeldTerminalOpens(s, teamKey, workspaceId));
}

/** Whether the team has a workspace focused (its focused one, else its first root, is open). */
export function useHasFocusedWorkspace(teamKey: string): boolean {
  return useViewerStore((s) => {
    const id = activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    return s.workspaces[teamKey]?.some((w) => w.id === id) ?? false;
  });
}

/** Whether the CLI pane is on screen (one setting for every team, D39). */
export function useTerminalVisible(): boolean {
  return useViewerStore((s) => s.terminalVisible);
}

/** A team as text names it ({@link teamLabelAmong}): its name, or `org › team` when an open team
 *  of another org has the same name. Null for null. */
export function useTeamLabel(teamKey: string | null): string | null {
  return useViewerStore((s) => (teamKey === null ? null : teamLabelAmong(teamKey, s.openedTeams)));
}

/** The CLI's layout mode, one for every team (`null` until first settled — callers
 *  apply their own default, e.g. `?? "viewer"` in bottom chrome or `?? "shared"` in
 *  the host). */
export function useLayoutMode(): LayoutMode | null {
  return useViewerStore((s) => s.layoutMode);
}

/** The active foreground slot's preset, or `undefined` when never arranged.
 *  Callers apply their own default (e.g. `?? "columns"`). */
export function useActiveForegroundPreset(teamKey: string) {
  return useViewerStore((s) => activeForegroundLayout(s, teamKey)?.preset);
}

/** How many tiles the active foreground slot holds. A primitive reduction over
 *  the (freshly-built) tile list, so no equality wrapper is needed. */
export function useActiveForegroundTileCount(teamKey: string): number {
  return useViewerStore((s) => activeForegroundTiles(s, teamKey).length);
}

/** Whether the epic-repos overlay is open for a workspace (false when none). */
export function useEpicReposOpen(workspaceId: string | null | undefined): boolean {
  return useViewerStore((s) => (workspaceId ? (s.epicReposOpen?.[workspaceId] ?? false) : false));
}

/** Whether the story HUD is shown for a workspace. */
export function useStoryHudShown(workspaceId: string): boolean {
  return useViewerStore((s) => s.storyHudShown[workspaceId] ?? false);
}

/** Whether the source-diff surface is shown for the terminal tab identified by
 *  `sessionId`. Per-tab visibility (DESIGN-per-tab-primitives.md §3.2). An
 *  empty `sessionId` (no active tab yet) always reads false. */
export function useSourceDiffShown(sessionId: string | undefined): boolean {
  return useViewerStore((s) =>
    sessionId ? (s.sourceDiffShownBySession[sessionId] ?? false) : false,
  );
}

/** Whether the worktree code editor is open for the terminal tab identified by
 *  `sessionId`. Per-tab visibility. An empty `sessionId` reads false. */
export function useWorktreeEditorOpen(sessionId: string | undefined): boolean {
  return useViewerStore((s) =>
    sessionId ? (s.worktreeEditorOpenBySession[sessionId] ?? false) : false,
  );
}

/** SW-TP CAP-6: imperative wrappers for the per-tab toolbar's icon toggles.
 *  Keeps the CLI seam at this adapter so caller components don't import
 *  `useViewerStore` directly (arch-boundary rule). */
export function toggleSourceDiffShownAction(opts: {
  sessionId: string;
  workspaceId: string;
  teamKey: string;
  shown?: boolean;
}): void {
  useViewerStore.getState().toggleSourceDiffShown(opts);
}

export function toggleWorktreeEditorOpenAction(opts: {
  sessionId: string;
  workspaceId: string;
  teamKey: string;
  shown?: boolean;
}): void {
  useViewerStore.getState().toggleWorktreeEditorOpen(opts);
}

export function toggleStoryHudShownAction(workspaceId: string): void {
  useViewerStore.getState().toggleStoryHudShown(workspaceId);
}

// --- Terminal-panels core (workspace-terminal-panels.tsx) ------------------

/** A workspace's terminal tabs in strip order (the store's frozen empty
 *  sentinel when it has none — stable identity either way). */
export function useWorkspaceTabsSlice(teamKey: string, workspaceId: string): TerminalTab[] {
  return useViewerStore((s) => selectWorkspaceTabs(s, teamKey, workspaceId));
}

/** The active tab id for a specific workspace. */
export function useWorkspaceActiveTab(teamKey: string, workspaceId: string): string | undefined {
  return useViewerStore((s) => selectActiveTerminalTab(s, teamKey, workspaceId));
}

/** Every open workspace's tabs, by team and workspace (stable identity). */
export function useTerminalTabIndex(): TerminalTabIndex {
  return useViewerStore(selectTerminalTabIndex);
}

/** Every running tab of the open workspaces — shown, then Hidden — by team and
 *  workspace (stable identity): the index "which sessions run here" reads. */
export function useRunningTerminalTabIndex(): TerminalTabIndex {
  return useViewerStore(selectRunningTerminalTabIndex);
}

/** Every minimized workspace's tabs, by team and workspace (stable identity). */
export function useMinimizedTerminalTabIndex(): TerminalTabIndex {
  return useViewerStore(selectMinimizedTerminalTabIndex);
}

/** One team's open workspaces' tabs, by workspace (stable identity). */
export function useTeamTerminalTabs(teamKey: string): Record<string, TerminalTab[]> | undefined {
  return useViewerStore((s) => selectTerminalTabIndex(s)[teamKey]);
}

/** SW-EW-46: a workspace's paned tab ids (grid cells) in pane order. */
export function useWorkspacePanedTabIds(teamKey: string, workspaceId: string): readonly string[] {
  return useViewerStore((s) => workspacePanedTabIds(s, teamKey, workspaceId));
}

/** SW-EW-46: whether this workspace should render its terminals as a live grid
 *  right now (expanded AND paned). Atomic boolean — no equality wrapper needed. */
export function useShouldRenderTabGrid(teamKey: string, workspaceId: string): boolean {
  return useViewerStore((s) => shouldRenderTabGrid(s, teamKey, workspaceId));
}

/** The workspace expanded over the team's other tiles (D68): its focused one while the global
 *  expanded setting is on and it has two or more tiles; null otherwise. */
export function useExpandedWorkspaceId(teamKey: string): string | null {
  return useViewerStore((s) => selectExpandedWorkspaceId(s, teamKey));
}

/** SW-EW-46 (D1): whether this workspace is expanded (zoomed solo or the lone tile)
 *  and may therefore grid its terminals. Resolved through the SAME
 *  `expandedWorkspaceId` the render gate uses, so the dial's visibility can never
 *  disagree with whether a grid would actually render. */
export function useIsExpandedWorkspace(teamKey: string, workspaceId: string): boolean {
  return useViewerStore((s) => expandedWorkspaceId(s, teamKey) === workspaceId);
}

/** SW-EW-46: the workspace's tab-grid preset (defaults to `columns`). */
export function useWorkspaceTabGridPreset(teamKey: string, workspaceId: string): LayoutPreset {
  return useViewerStore((s) => workspaceTabGridPreset(s, teamKey, workspaceId));
}

/** The sizes this viewer dragged the workspace's tab grid to, stamped with their
 *  preset (undefined when it lays out evenly). A stored reference. */
export function useTabGridLayout(teamKey: string, workspaceId: string) {
  return useViewerStore((s) => s.tabGridLayout[selectWorkspaceKey(s, teamKey, workspaceId)]);
}

/** The team's workspace showing its terminals as a tab grid right now, or null. */
export function useTabGridWorkspaceId(teamKey: string): string | null {
  return useViewerStore((s) => expandedGridWorkspaceId(s, teamKey));
}

/** The preset the team's tab grid is laid out with, or null when no workspace
 *  shows all its tabs at once. */
export function useTabGridPresetOf(teamKey: string): LayoutPreset | null {
  return useViewerStore((s) => {
    const workspaceId = expandedGridWorkspaceId(s, teamKey);
    return workspaceId ? workspaceTabGridPreset(s, teamKey, workspaceId) : null;
  });
}

/** The external editor chosen in Settings: its id (`""` when none) and, for
 *  `custom`, its command. */
export function useEditorChoice(): { id: string; customCmd: string } {
  const id = useViewerStore((s) => s.settings.editor);
  const customCmd = useViewerStore((s) => s.settings.editorCustomCmd);
  return { id, customCmd };
}

/** Terminal font-size setting (named step). */
export function useTerminalFontSize() {
  return useViewerStore((s) => s.settings.terminalFontSize);
}

/** Terminal font-size override in px. */
export function useTerminalFontSizePx() {
  return useViewerStore((s) => s.settings.terminalFontSizePx);
}

/** The team's focused workspace id. */
export function useFocusedWorkspaceId(teamKey: string): string | undefined {
  return useViewerStore((s) => s.activeWorkspace[teamKey]);
}

/** The team's tray grouping mode (caller compares to `"grouped"`). */
export function useTrayMode(teamKey: string) {
  return useViewerStore((s) => s.trayMode[teamKey]);
}

/** The selected epic rail key (empty string when none selected). */
export function useSelectedRailKey(teamKey: string): string {
  return useViewerStore((s) => s.selectedEpicDirName[teamKey] ?? "");
}

/** The team's stored rail-key-by-workspace index (stored reference). */
export function useRailKeyIndex(teamKey: string) {
  return useViewerStore((s) => s.railKeyByWorkspaceId[teamKey]);
}

/** The team's flat foreground layout (stored reference). */
export function useFlatForegroundLayout(teamKey: string) {
  return useViewerStore((s) => s.foregroundLayout[teamKey]);
}

/** A rail's foreground layout in grouped mode (stored reference). */
export function useRailForegroundLayout(teamKey: string, railKey: string) {
  return useViewerStore((s) => s.foregroundLayoutByRail[teamKey]?.[railKey]);
}

/** The team's shared workspace order (pills and Agents-pane cards), which its
 *  foreground tiles follow. Cached per team while its slices are unchanged, and
 *  compared shallowly so an equal recompute does not re-render. Never mutate it. */
export function useTeamTileOrder(teamKey: string): readonly string[] {
  return useViewerStore(useShallow((s) => teamTileOrder(s, teamKey)));
}

// --- Host CLI reads (team-workspace-view.tsx) ------------------------------

/** The team's active workspace id, resolved against the workspace list. */
export function useActiveWorkspaceId(teamKey: string): string | undefined {
  return useViewerStore((s) => activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces));
}

/** The pre-record "opening" workspace id, or `undefined` once the record lands.
 *  Primitive result → re-renders only on the actual open→landed transition. */
export function useOpeningWorkspaceId(teamKey: string): string | undefined {
  return useViewerStore((s) =>
    resolveOpeningWorkspaceId(s.openingWorkspaces[teamKey], s.workspaces[teamKey] ?? []),
  );
}

/** The viewer's share of the column beside the CLI, one for every team. */
export function useTerminalSplitRatio(): number {
  return useViewerStore((s) => s.terminalSplitRatio);
}

/** The team's soft-closed (minimized) workspaces map (stored reference). */
export function useSoftClosedForTeam(teamKey: string) {
  return useViewerStore((s) => s.softClosedWorkspaces[teamKey]);
}

/** How many workspaces the team has. */
export function useWorkspaceCount(teamKey: string): number {
  return useViewerStore((s) => s.workspaces[teamKey]?.length ?? 0);
}

/** The projected terminal layout of every admitted runtime member. */
export function useTerminalLayout(): TerminalLayoutProjection {
  return useViewerStore((s) => s.terminalLayout);
}

/** The team of the open workspace holding `sessionId`'s tab. */
export function useSessionTabTeamKey(sessionId: string): string | undefined {
  return useViewerStore(
    (s) => findTabLocationBySessionId(selectTerminalTabIndex(s), sessionId)?.teamKey,
  );
}

/** The open workspace holding `sessionId`'s tab (the stored reference). */
export function useSessionTabWorkspace(sessionId: string): Workspace | undefined {
  return useViewerStore((s) => {
    const at = findTabLocationBySessionId(selectTerminalTabIndex(s), sessionId);
    return at ? s.workspaces[at.teamKey]?.find((w) => w.id === at.workspaceId) : undefined;
  });
}

/** The name of `sessionId`'s tab. */
export function useSessionTabName(sessionId: string): string | undefined {
  return useViewerStore(
    (s) => findTabLocationBySessionId(selectTerminalTabIndex(s), sessionId)?.tab.name,
  );
}
