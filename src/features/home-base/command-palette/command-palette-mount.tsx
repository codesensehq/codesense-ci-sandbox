// SPDX-License-Identifier: MIT
import { declaresSlot, revealSlot } from "@/features/shell/slots/reveal-slot";
// SW-HB-6 — the ⌘⇧P command palette mount.
//
// The single production seam that feeds the pure index (command-palette-items)
// live store data, owns the async on-disk epic-file walk, and dispatches a
// picked row through EXISTING app handlers. Mounted app-globally in App.tsx
// (SW-HB-6 G2): the palette action is `scope: "global"`, so it must work from
// the Home Base hub AND inside a team view — unlike the team-scoped ⌘⇧K / ⌘O
// palettes that mount inside `TeamWorkspaceView`.
//
// Non-blocking (SW-HB-6 G4): teams / repos / actions / recent files come from
// synchronous in-memory state and render on first paint. The epic-file index is
// a fire-and-forget per-root listing from the context index (no network — AC-6),
// fanned out across the active org's distinct, non-archived teams with
// `Promise.allSettled` and guarded by a monotonic request id (nonce) so a slow
// listing for a just-closed palette / switched org can't apply late.

import { useEffect, useMemo, useState } from "react";

import { QuickOpenDialog } from "@/features/viewer/components/quick-open-dialog";
import { logger } from "@/shared/lib/logger";
import { buildTeamKey, parseTeamKey, useActiveTeamKey } from "@/shared/lib/team-key";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { useActiveOrg } from "@/shared/stores/use-active-org";
import { numericPrincipal, useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  fileListErrorMessage,
  isNotIndexedError,
  requestContentFileIndex,
} from "@/shared/lib/pipeline-graph/content-scan-queue";
import { toastError } from "@/shared/lib/with-error-toast";

import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { paneIcon } from "@/features/shell/right-pane/panes/pane-chrome";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import {
  findShortcutAction,
  resolveBindings,
  useDynamicShortcutActions,
} from "@/shared/lib/shortcuts/registry";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { panePanelTitle } from "@/features/shell/right-pane/panes/use-register-pane-panels";
import { openAppSettings } from "@/features/settings/lib/open-settings";
import { openSourceRepos } from "@/features/dashboard/source-repos-virtual-tab";
import { openManageTeams } from "@/features/shell/manage-teams-virtual-tab";
import { openMachinesPanel } from "@/features/machines/lib/machine-actions";
import { openRoutines } from "@/features/routines/routines-virtual-tab";
import { openAgents } from "@/features/agents/agents-virtual-tab";
import { openScratchSpace } from "@/features/scratch-space/scratch-space-virtual-tab";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { useTeamMachines } from "@/features/machines/stores/machines-store";

import { useEnterTeam } from "../lib/enter-team";
import {
  buildEpicFileItems,
  buildStaticItems,
  dispatchSelection,
  encodeSelection,
  type CommandPaletteHandlers,
  type EpicFileEntry,
  type PaletteCommand,
  type PalettePane,
  type RecentEpicEntry,
  type SwitchableAccount,
} from "./command-palette-items";

/** Cap the session-recent rows so a long-lived session doesn't flood the empty
 *  state; the full epic-file index (searchable) covers the rest. */
const MAX_RECENT = 8;

export function CommandPaletteMount() {
  const open = useUiStore((s) => s.commandPaletteOpen);
  const setOpen = useUiStore((s) => s.setCommandPaletteOpen);

  const activeOrg = useActiveOrg();
  // FULL team set (SW-HB-6 G1/AC-2) — curation must NOT hide a team from the
  // palette, so read `org.teams` directly, never `visibleTeams(...)`. Memoized
  // on `activeOrg` so the `?? []` fallback doesn't hand a fresh array to the
  // index/effect deps on every render (which would re-run the walk each render,
  // and loop when no org is active).
  const teams = useMemo(() => activeOrg?.teams ?? [], [activeOrg]);
  const activeTeamName = useOrgStore((s) => s.activeTeamName);
  const activeTeamKey = useActiveTeamKey();
  const machines = useTeamMachines(activeTeamKey);
  // Adding the FIRST machine is a command away too; only a build without the
  // machine registry (not wired yet) offers neither.
  const canAddMachine = machines !== null;

  // Custom right panes are offered while a team's right-pane host is mounted (it
  // owns the listing and clears it on unmount); opening one reveals the pane.
  const paneListing = usePaneStore((s) => s.listing);
  const paneTeamKey = usePaneStore((s) => s.teamKey);
  const panes = useMemo<PalettePane[]>(
    () =>
      paneListing && activeTeamName && paneTeamKey === activeTeamKey
        ? paneListing.panes
            .filter((p) => declaresSlot(p, "right-pane"))
            .map((p) => ({
              panelId: p.panelId,
              title: panePanelTitle(p, activeTeamName),
              layer: p.scope === "private" ? "Private" : (p.team ?? ""),
            }))
        : [],
    [paneListing, paneTeamKey, activeTeamKey, activeTeamName],
  );

  // Active slot-dashboard commands, each with its current chord.
  const dashboardActions = useDynamicShortcutActions((s) => s.actions);
  const shortcuts = useShortcutsStore((s) => s.shortcuts);
  const commands = useMemo<PaletteCommand[]>(() => {
    const bindings = resolveBindings(shortcuts);
    return dashboardActions
      .filter((a) => a.command?.active)
      .map((a) => ({
        actionId: a.id,
        label: a.label,
        dashboardTitle: a.command!.dashboardTitle,
        icon: paneIcon(a.command!.icon),
        chord: formatKeystrokeDisplay(bindings.get(a.id)?.keystroke ?? ""),
      }));
  }, [dashboardActions, shortcuts]);

  // The Routines row carries the needs-you count the title-bar entry keeps live.
  const routinesNeedingYou = useRoutinesStore((s) => s.needsYouCount);

  const profiles = useLocalProfileStore((s) => s.status);
  const openedViewerPaths = useViewerStore((s) => s.openedViewerPaths);
  // The canonical hub→team entry seam (SW-HB-5): setActiveTeam + leave the hub,
  // shared with the teams-panel cards / next-step banner / resume panel so the
  // "leave the hub on entry" sequence lives in ONE place.
  const enterTeam = useEnterTeam();

  // Inactive accounts only — switching to the active one is a no-op (AC-5).
  const accounts = useMemo<SwitchableAccount[]>(
    () =>
      (profiles?.profiles ?? [])
        .filter((p) => p.principal !== profiles?.active?.principal)
        .map((p) => ({ userId: numericPrincipal(p.principal), label: p.displayName })),
    [profiles],
  );

  // Session-recent files (SW-HB-6 R2): each team's paths are MRU-ordered
  // (`openedViewerPaths` is per-team MRU), grouped by team and capped. Best-
  // effort recency — cross-team interleaving isn't timestamped, so this is
  // per-team-recent, not a strict global MRU. Scoped to the active org.
  const recentEpics = useMemo<RecentEpicEntry[]>(() => {
    if (activeOrg === null) return [];
    const out: RecentEpicEntry[] = [];
    for (const [teamKey, paths] of Object.entries(openedViewerPaths ?? {})) {
      const { orgSlug, teamName } = parseTeamKey(teamKey);
      if (orgSlug !== activeOrg.slug || teamName === null) continue;
      const prefix = `${pathToTeamContentDirSync(activeOrg.org_root, teamName)}/`;
      for (const fullPath of paths) {
        out.push({
          teamName,
          fullPath,
          relativePath: fullPath.startsWith(prefix) ? fullPath.slice(prefix.length) : fullPath,
        });
      }
    }
    return out.slice(0, MAX_RECENT);
  }, [openedViewerPaths, activeOrg]);

  const staticItems = useMemo(
    () =>
      buildStaticItems({
        teams,
        accounts,
        recentEpics,
        canAttach: teams.length > 0,
        canAddMachine,
        routinesNeedingYou,
        panes,
        commands,
      }),
    [teams, accounts, recentEpics, canAddMachine, routinesNeedingYou, panes, commands],
  );

  // --- Async epic-file index (fire-and-forget on open; disk-only, AC-6) -----
  const [epicFiles, setEpicFiles] = useState<EpicFileEntry[]>([]);
  const [walking, setWalking] = useState(false);

  useEffect(() => {
    // Synchronizing an EXTERNAL async system (the disk walk) into React state is
    // exactly what this effect is for; the synchronous status writes are the
    // pending/short-circuit legs of that sync, not a render loop.
    /* eslint-disable react-hooks/set-state-in-effect */
    // Only index while open; clear on close so a re-open starts fresh and stale
    // rows don't linger in memory.
    if (!open || activeOrg === null) {
      setEpicFiles([]);
      setWalking(false);
      return;
    }
    // Out-of-order / unmount guard: a per-run `cancelled` flag the cleanup flips.
    // React runs a run's cleanup before the next run's body (and on unmount), so a
    // late walk can't setState after close or overwrite a newer org's results.
    let cancelled = false;
    setWalking(true);
    // Clear the prior walk's results immediately so a re-run while OPEN (an org
    // switch) can't render the previous org's epic-file rows until the new walk
    // resolves. Without this, only the close branch cleared them.
    setEpicFiles([]);
    /* eslint-enable react-hooks/set-state-in-effect */
    const orgRoot = activeOrg.org_root;
    // One listing per DISTINCT, non-archived root, through the shared per-root cache
    // (SV-PBF-267): re-opening at the same index generation makes no backend call.
    // Fault-isolated via allSettled — one unreadable content dir must not blank the index.
    const listed = teams.filter((t) => !t.archived);
    const rootOf = (name: string) => pathToTeamContentDirSync(orgRoot, name);
    const distinct = new Map<string, string>();
    for (const t of listed) if (!distinct.has(rootOf(t.name))) distinct.set(rootOf(t.name), t.name);
    Promise.allSettled(
      [...distinct].map(async ([root, teamName]) => {
        const files = await requestContentFileIndex(root);
        return files.map(
          (f): EpicFileEntry => ({
            teamName,
            name: f.name,
            relativePath: f.relativePath,
            fullPath: f.fullPath,
          }),
        );
      }),
    )
      .then((settled) => {
        if (cancelled) return; // stale/closed — drop
        const merged: EpicFileEntry[] = [];
        const failures: unknown[] = [];
        for (const r of settled) {
          if (r.status === "fulfilled") merged.push(...r.value);
          else {
            failures.push(r.reason);
            logger.warn("home-base", `command-palette epic-file listing failed: ${r.reason}`);
          }
        }
        // Every root refused: say why instead of showing an empty palette.
        if (failures.length > 0 && failures.length === settled.length) {
          const reason = failures.find(isNotIndexedError) ?? failures[0];
          toastError(
            "Couldn't list team files: ",
            isNotIndexedError(reason) ? new Error(fileListErrorMessage(reason)) : reason,
          );
        }
        setEpicFiles(merged);
        setWalking(false);
      })
      .catch((e) => {
        if (cancelled) return;
        logger.error("home-base", `command-palette index failed: ${e}`);
        setWalking(false);
      });

    // Flip `cancelled` on unmount / dep-change so a late walk can't setState after close.
    return () => {
      cancelled = true;
    };
  }, [open, activeOrg, teams]);

  const items = useMemo(() => {
    // Dedupe epic-file rows against the recent rows already shown (R2).
    const recentIds = new Set(
      recentEpics.map((r) =>
        encodeSelection({ kind: "epic", teamName: r.teamName, filePath: r.fullPath }),
      ),
    );
    return [...staticItems, ...buildEpicFileItems(epicFiles, recentIds)];
  }, [staticItems, epicFiles, recentEpics]);

  // --- Dispatch: every handler is an EXISTING seam (SW-HB-6 AC-5) -----------
  const handlers = useMemo<CommandPaletteHandlers>(() => {
    const enterSelectedTeam = (teamName: string) => {
      if (!activeOrg || !activeOrg.teams.some((team) => team.name === teamName && !team.archived)) {
        return;
      }
      // Picking a palette row explicitly reveals a curated-out team. History
      // navigation still skips teams the user has hidden.
      const org = useOrgStore.getState();
      const shown = org.shownTeamsByOrgSlug[activeOrg.slug];
      if (shown && !shown.includes(teamName)) {
        org.setShownTeams(activeOrg.slug, [...shown, teamName]);
      }
      enterTeam(teamName);
    };
    return {
      // Reuse the canonical `useEnterTeam` seam (SW-HB-5) — shared with the
      // teams-panel cards / next-step banner / resume panel (AC-2), so nothing
      // drifts on the "activate team + leave the hub" sequence.
      openTeam: (teamName) => enterSelectedTeam(teamName),
      attachRepo: (teamName) => {
        if (activeOrg === null) return;
        // Activate the team first so the source-repos tab targets it, then open it
        // in add mode (the tab operates on the active team).
        enterSelectedTeam(teamName);
        openSourceRepos("add");
      },
      openEpicFile: (teamName, filePath) => {
        if (activeOrg === null) return;
        enterSelectedTeam(teamName);
        // Guard the phantom-tab case: if the team didn't activate (removed since
        // palette-open), skip the open rather than commit a tab under a dead key.
        // (Leaving the hub on a stale team is the shared seam's consistent
        // behavior across every hub-entry point, not palette-specific.)
        if (useOrgStore.getState().activeTeamName !== teamName) return;
        // repoPath is best-effort for nav-reveal only; the tab commit keys off
        // teamKey + filePath, so a cross-team open still lands the file as a tab
        // that renders when the team's view mounts (revealOpenedFile no-ops
        // safely on an unresolved repoPath).
        useViewerStore.getState().openFile(buildTeamKey(activeOrg.slug, teamName), "", filePath);
      },
      switchAccount: (userId) => void useLocalProfileStore.getState().select(String(userId)),
      createTeam: () => openManageTeams("create"),
      openPane: (panelId) => revealSlot("right-pane", { dashboard: panelId }),
      runCommand: (actionId) => findShortcutAction(actionId)?.run(),
      // Reuse the canonical settings opener — deep-links straight to the
      // Shortcuts section where the nav axes are listed with their bindings.
      openShortcuts: () => openAppSettings("shortcuts"),
      openRoutines: () => openRoutines(),
      openAgents: () => openAgents(),
      openScratchSpace: () => openScratchSpace(),
      addMachine: () => openMachinesPanel({ mode: "add" }),
    };
  }, [activeOrg, enterTeam]);

  // The generic "Attach a source repo…" action needs a target team — prefer the
  // active team, else the org's first team (null → the action no-ops).
  const attachTargetTeam = activeTeamName ?? teams[0]?.name ?? null;

  return (
    <QuickOpenDialog
      open={open}
      onClose={() => setOpen(false)}
      onSelect={(id) => dispatchSelection(id, handlers, attachTargetTeam)}
      items={items}
      loading={walking}
      placeholder="Jump to a team, repo, or epic — or run an action…"
      emptyLabel="No matches"
      loadingLabel="Indexing…"
    />
  );
}
