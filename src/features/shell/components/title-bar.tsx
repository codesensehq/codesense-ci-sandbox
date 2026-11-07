// SPDX-License-Identifier: MIT
import { CliBottomChrome } from "@/features/viewer/components/cli-bottom-chrome";
import { ViewerTabs } from "@/features/viewer/components/viewer-tabs";
import { startDragging } from "@/shared/lib/titlebar";
import { NO_TEAM_SENTINEL, useActiveTeamKey } from "@/shared/lib/team-key";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { useIsConsole } from "@/shared/hooks/use-is-console";
import { useTeamPipelineRepoPath } from "@/shared/hooks/use-team-pipeline-repo-path";
import { IdentityMenu } from "@/features/shell/components/identity-menu";
import { TerminalServersTitleBarNotice } from "@/features/machines/components/terminal-servers-notice";

/** Width (CSS px) of the macOS traffic-light zone (close/minimize/zoom). */
const TRAFFIC_LIGHT_WIDTH = 78;

/**
 * App-wide title bar. Single 40px chrome row for the whole app.
 *
 * Layout (left → right):
 *   - 78px drag-only spacer reserved for macOS Overlay traffic lights.
 *   - SW-OC-28: the identity menu (`context repo / team ▾`) — the one place to
 *     switch context repo or team and to reach Private, agent
 *     definitions, routines, Machines and Settings (agent accounts have no menu row yet,
 *     SV-ENG-78).
 *   - Middle (flex-1): the viewer's doc tabs (`<ViewerTabs />`) in
 *     viewer/shared modes; in console (full-screen CLI) mode it instead
 *     hosts the **workspace switcher** — the single `WorkspaceTray` is
 *     portaled into a slot here (SW-EW-34 v2), so full CLI reads as a
 *     terminal app: no markdown-viewer/doc tabs, the workspace chips up top.
 *   - Right: in console, the CLI controls (`<CliBottomChrome />`:
 *     layout preset · full-screen EXIT · epic-repos) ride here behind a divider
 *     so the EXIT toggle is always visible top-right.
 *
 * Drag region: the outer container is `titlebar-drag` + `startDragging`;
 * interactive children are auto-excluded from drag via the
 * `-webkit-app-region: no-drag` CSS rule on `.titlebar-drag button`.
 */
export function TitleBar() {
  // T.6: gate the `ViewerTabs` strip on "any workspace focused in the active
  // team's bar" instead of the retired `selectedRepoPath` + `viewMode` compound.
  const teamKey = useActiveTeamKey();
  // SLW-FX-11: also read the workspace-derived synthetic `repoPath`
  // (`contextRepoPath`) from the SAME call — it's viewer-state-only and
  // hydrated before the window shows, so it's a valid pre-discovery fallback
  // for the `ViewerTabs` prop (see the tab gate below).
  const { workspace, repoPath: contextRepoPath } = useActiveWorkspaceContext();
  // SW-EW-24 / SW-FX-37: tabs are team-keyed. Resolve the team-stable
  // pipeline repoPath (the team's first source repo's synthetic, regardless
  // of which workspace is focused) for the gate + the action-caller prop.
  // Identity `repoPath` ("" for a slug-less workspace) would let an epic
  // integration workspace's team-keyed tabs vanish — `tabRepoPath` resolves
  // independently of `sourceRepoSlug` so they stay mounted.
  const tabRepoPath = useTeamPipelineRepoPath(workspace, teamKey);
  // SLW-FX-11 (bug 2): the cold-open render-gate signal. `tabRepoPath` is
  // org-store-derived and stays "" until `useOrgDiscovery` finishes its async
  // disk crawl (well after the window paints), so gating the strip on it hid
  // the doc tabs on first paint despite the tab DATA being viewer-store-only
  // and already hydrated. `hasTabs` reads the EXACT slice `ViewerTabs` renders
  // (`openTabs[teamKey]`) — workspace-kind-agnostic (covers epic workspaces,
  // which carry no `sourceRepoSlug`) and available before `show()`. It is the
  // truest "is there anything to show" signal, and it doubly serves SW-EW-24
  // AC-6: a genuinely repo-less team with no open tabs still renders nothing.
  const hasTabs = useViewerStore((s) => (s.openTabs?.[teamKey]?.length ?? 0) > 0);
  // SLW-FX-11: the `ViewerTabs` PROP (not the mount trigger) — org-store
  // `tabRepoPath` when discovery has landed, else the workspace-derived
  // synthetic fallback (`contextRepoPath`, action-readiness during the
  // pre-discovery window), else "" (epic / no slug yet). `contextRepoPath` is
  // deliberately PROP-ONLY, never a mount condition: the mount rides
  // `tabRepoPath || hasTabs` (below), so a slug-bearing workspace with zero
  // open tabs can't mount an empty strip pre-discovery — keeping SW-EW-24 AC-6
  // byte-for-byte.
  const effectiveTabRepoPath = tabRepoPath || contextRepoPath || "";

  // SW-EW-34 v2: console (full-screen CLI) is its own immersive terminal mode —
  // hide ALL markdown-viewer chrome (doc tabs) and host the workspace switcher
  // here instead (the single WorkspaceTray, portaled into the slot below by the
  // terminal view). Doc tabs render only in viewer/shared modes.
  const isConsole = useIsConsole();
  // Callback-ref into ui-store so the terminal view can portal the tray here.
  const setConsoleSwitcherSlot = useUiStore((s) => s.setConsoleSwitcherSlot);

  return (
    <div
      data-testid="title-bar"
      className="titlebar-drag flex-shrink-0 flex flex-col border-b border-border bg-background"
    >
      {/* Traffic lights, identity, the single-row switcher/doc tabs and controls.
          The row grows with content while native controls keep their clearance. */}
      <div
        onMouseDown={(e) => {
          // Skip the macOS traffic-light zone (close/minimize/zoom) so
          // startDragging doesn't steal the mousedown from native controls.
          if (e.clientX < TRAFFIC_LIGHT_WIDTH) return;
          startDragging(e);
        }}
        className="@container/titlebar min-h-10 flex items-start"
      >
        {/* macOS traffic-light clearance. */}
        <div
          style={{ width: TRAFFIC_LIGHT_WIDTH }}
          className="flex-shrink-0 h-10"
          aria-hidden="true"
        />

        {/* SW-OC-28: whose space you're in, anchored where macOS users look for it;
            tabs or pills flow from it to the right. Capped so a long name can't
            starve the lane — it truncates instead. Pinned to row 1. */}
        <div className="flex min-w-0 max-w-64 flex-shrink-0 h-10 items-center self-start">
          <IdentityMenu />
        </div>
        <div className="mx-1.5 flex-shrink-0 h-10 self-start flex items-center" aria-hidden="true">
          <div className="h-4 w-px bg-border/70" />
        </div>

        {/* Middle region — doc tabs (viewer/shared) OR, in console, the slot the
            single WorkspaceTray is portaled into (the workspace switcher). The
            identity menu sits before it; everything else rare lives in that
            menu, so the lane gets the rest of the row. */}
        <div
          data-testid="title-bar-middle"
          className={`flex-1 min-w-0 min-h-10 flex ${isConsole ? "items-start" : "items-end"}`}
        >
          {/* SLW-FX-11 (bug 2): the `ViewerTabs` strip mounts when there's
              something to show — a resolved repo (`tabRepoPath`) OR persisted open
              tabs (`hasTabs`, viewer-state-only, hydrated before `show()`, covers
              epic workspaces and a repo-less team whose `workspace` is null). The
              prop still feeds the best-available key (`effectiveTabRepoPath`), so
              `tabRepoPath` wins once orgs load. No tabs and no repo → no empty strip
              (SW-EW-24 AC-6). */}
          {!isConsole && (hasTabs || (workspace !== null && tabRepoPath)) && (
            <ViewerTabs repoPath={effectiveTabRepoPath} />
          )}
          {isConsole && (
            <div
              ref={setConsoleSwitcherSlot}
              data-testid="console-switcher-slot"
              // Align the shared single-row strip with identity and controls.
              className="flex-1 min-w-0 flex items-start"
            />
          )}
        </div>

        {/* CLI controls and Inbox keep their native top position. */}
        {/* Right gutter (`pr-3`) gives the last control the same breathing room the
            traffic lights get on the left, instead of hugging the window edge. */}
        <div className="flex flex-shrink-0 h-10 items-center self-start gap-1.5 pl-1.5 pr-3">
          {/* SW-EW-34 v2: in console the CLI controls (layout · EXIT ·
              epic-repos) ride the right cluster behind a divider, so the
              full-screen EXIT toggle is always visible top-right. The divider rides
              with the chrome (`leadingDivider`) so it can't strand alone when the
              chrome self-gates to null. */}
          {isConsole && <CliBottomChrome placement="titlebar" leadingDivider trailingDivider />}
          {/* SW-AGR-19: terminal servers older than this app — only while some
              are and the user has not dismissed that set. */}
          {teamKey !== NO_TEAM_SENTINEL && <TerminalServersTitleBarNotice teamKey={teamKey} />}
        </div>
      </div>
    </div>
  );
}
