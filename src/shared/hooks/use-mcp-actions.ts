// SPDX-License-Identifier: MIT
import { mcpNavLens } from "@/shared/lib/nav-lens";
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { cliChromeOf, cliChromeState, useViewerStore } from "@/shared/stores/viewer-store";
import { applyCliChromePatch, invalidCliChromePatchFields } from "@/shared/stores/cli-chrome";
import {
  coerceRightPaneLayout,
  reconcileLayoutToBuiltins,
} from "@/features/shell/right-pane/right-pane-layout";
import { logger } from "@/shared/lib/logger";
import { useOrgStore } from "@/shared/stores/org-store";
import { getActiveTeamKey, parseTeamKey } from "@/shared/lib/team-key";
import {
  currentPrivateAuthorityEpoch,
  onPrivateAuthorityRetired,
} from "@/shared/lib/private-terminal-authority";
import { ensureTeamMachines } from "@/features/machines/stores/machines-store";
import { terminalLayoutMemberSnapshots } from "@/tauri/commands";
import { clientBootId } from "@/shared/lib/terminal-layout";
import { REMOTE_TERMINAL_REVEAL_EVENT, type RemoteTerminalRevealPayload } from "@/tauri/events";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { machineWorkspace } from "@/features/machines/stores/machine-projection-selectors";
import { type RetiredWorkspace, retiredWorkspaceIds } from "./retired-workspace";

/** The tab an MCP reveal names, revalidated against the authorized projection:
 *  its session id, or `null` when the evidence no longer holds. Never grants
 *  runtime authority. */
function revealedRemoteTab(payload: RemoteTerminalRevealPayload): string | null {
  const { outcome, teamKey, workspaceId, privateAuthorityEpoch } = payload;
  if (
    !privateAuthorityEpoch ||
    currentPrivateAuthorityEpoch() !== privateAuthorityEpoch ||
    !outcome.headed ||
    !["opened", "alreadyLive"].includes(outcome.status) ||
    !outcome.target ||
    outcome.target.member.slot !== "remote" ||
    outcome.workspaceId !== workspaceId ||
    !outcome.handle ||
    !outcome.requestId
  )
    return null;
  const workspace = useViewerStore
    .getState()
    .workspaces[teamKey]?.find((row) => row.id === workspaceId);
  if (!workspace) return null;
  const key = workspaceKey(teamKey, workspace);
  if (key !== outcome.workspaceKey) return null;
  const tab = machineWorkspace(teamKey, outcome.target, key)?.tabs.find(
    (row) => row.session === outcome.target?.session,
  );
  if (
    !tab ||
    tab.state.kind === "exited" ||
    tab.tags.remoteHandle !== outcome.handle ||
    tab.tags.remoteRequestId !== outcome.requestId
  )
    return null;
  return tab.session;
}

/** Keys that MCP is allowed to write into the viewer store. Terminal tabs are
 *  not viewer state: their hosts own them (`open_terminal_tab` opens one).
 *
 *  T.6: `selectedRepoPath` removed — MCP clients that previously wrote
 *  it should retarget to a future `switchWorkspace` write-path. */
const ALLOWED_MCP_KEYS = new Set([
  "activeTab",
  "openTabs",
  "drillPaths",
  "navLens",
  "navCollapsed",
  // D39: the CLI's chrome, one value for every team, merged field by field (see
  // `applyCliChromePatch`).
  "cliChrome",
  "rightPaneOpen",
  "rightPaneRatio",
  // SW-RP-1: whole structured layout (sections sent whole — deep_merge replaces
  // arrays wholesale, so partial section patches are not supported).
  "rightPaneLayout",
  "settings",
]);

/**
 * Listens for MCP state updates emitted from the Rust backend
 * and applies them directly to the Zustand viewer store.
 * This is the reverse of pushViewerState() — Rust writes state
 * back to the frontend via a single generic event.
 *
 * No feedback loop guard needed: pushViewerState() is a one-way
 * Tauri invoke command that stores data in Rust memory without
 * emitting any events back. The flow terminates naturally:
 * MCP set_viewer_state -> emit -> setState -> persistState -> pushViewerState -> STOP
 */
export function useMcpActions() {
  useEffect(() => {
    const unlisten = listen<{
      teamKey: string;
      workspaceId: string;
      before?: string;
      after?: string;
    }>("mcp:reorder-workspace", ({ payload }) => {
      const { teamKey, workspaceId, before, after } = payload;
      useViewerStore
        .getState()
        .reorderWorkspace(
          teamKey,
          workspaceId,
          before !== undefined ? { before } : { after: after! },
        );
    });
    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
  useEffect(() => {
    let live = true;
    let generation = 0;
    let activating = false;
    const supersede = () => {
      if (activating) return;
      generation++;
    };
    document.addEventListener("pointerdown", supersede, true);
    document.addEventListener("keydown", supersede, true);
    const stopAuthority = onPrivateAuthorityRetired(supersede);
    const stopOrg = useOrgStore.subscribe((next, previous) => {
      if (
        next.activeOrgSlug !== previous.activeOrgSlug ||
        next.activeTeamName !== previous.activeTeamName
      )
        supersede();
    });
    const stopWorkspace = useViewerStore.subscribe((next, previous) => {
      if (next.activeWorkspace !== previous.activeWorkspace) supersede();
    });
    const unlisten = listen<RemoteTerminalRevealPayload>(
      REMOTE_TERMINAL_REVEAL_EVENT,
      ({ payload }) => {
        if (
          !live ||
          !payload.outcome.headed ||
          !["opened", "alreadyLive"].includes(payload.outcome.status) ||
          !payload.privateAuthorityEpoch ||
          payload.privateAuthorityEpoch !== currentPrivateAuthorityEpoch()
        )
          return;
        supersede();
        const captured = generation;
        const isCurrent = () =>
          live &&
          captured === generation &&
          payload.privateAuthorityEpoch === currentPrivateAuthorityEpoch();
        const { orgSlug, teamName } = parseTeamKey(payload.teamKey);
        const accessible = () =>
          useOrgStore
            .getState()
            .orgs.some(
              (org) => org.slug === orgSlug && org.teams.some((team) => team.name === teamName),
            ) &&
          useViewerStore
            .getState()
            .workspaces[payload.teamKey]?.some((workspace) => workspace.id === payload.workspaceId);
        if (!accessible()) return;
        void Promise.all([
          // A machine saved moments ago may land in a read slower than the
          // usual wait; the reveal holds for it rather than missing it.
          ensureTeamMachines(payload.teamKey, payload.outcome.target?.member.machineId),
          terminalLayoutMemberSnapshots(clientBootId()),
        ])
          .then(([loaded, replies]) => {
            if (!loaded || !isCurrent() || !accessible()) return;
            useViewerStore.getState().applyLayout({ kind: "snapshot", replies });
            if (!isCurrent() || !revealedRemoteTab(payload)) return;
            // A reveal the user did not click switches no team: it focuses the workspace and
            // its tab in that team's memory, and shows the terminal only in the active team.
            activating = true;
            try {
              if (!accessible()) return;
              if (useOrgStore.getState().activeOrgSlug === orgSlug) {
                useViewerStore.getState().openTeam(payload.teamKey);
              }
              if (!accessible()) return;
              useViewerStore.getState().switchWorkspace(payload.teamKey, payload.workspaceId);
              if (getActiveTeamKey() === payload.teamKey) {
                useViewerStore.getState().revealTerminalInPlace();
              }
            } finally {
              activating = false;
            }
            const session = revealedRemoteTab(payload);
            if (!isCurrent() || !session) return;
            useViewerStore
              .getState()
              .setActiveTerminalTab(payload.teamKey, session, payload.workspaceId);
          })
          .catch((error: unknown) =>
            logger.warn("mcp", `Remote terminal reveal unavailable: ${String(error)}`),
          );
      },
    );
    return () => {
      live = false;
      supersede();
      stopAuthority();
      stopOrg();
      stopWorkspace();
      document.removeEventListener("pointerdown", supersede, true);
      document.removeEventListener("keydown", supersede, true);
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
  useEffect(() => {
    const unlisten = listen<Record<string, unknown>>("mcp:set-viewer-state", (event) => {
      logger.debug("mcp", "set-viewer-state received");
      // Filter to only allowed viewer state keys
      const filtered: Record<string, unknown> = {};
      for (const [key, value] of Object.entries(event.payload)) {
        if (ALLOWED_MCP_KEYS.has(key)) {
          filtered[key] = value;
        } else {
          logger.warn("mcp", `Dropped unsupported viewer state key: ${key}`);
        }
      }
      if (filtered.navLens && typeof filtered.navLens === "object") {
        for (const [teamKey, value] of Object.entries(filtered.navLens)) {
          const patch = mcpNavLens(value);
          if (patch) {
            useViewerStore.getState().setNavLens(teamKey, patch);
            useViewerStore.getState().setNavCollapsed(teamKey, false);
          }
        }
        delete filtered.navLens;
      }
      if (filtered.navCollapsed && typeof filtered.navCollapsed === "object") {
        for (const [teamKey, collapsed] of Object.entries(filtered.navCollapsed)) {
          if (typeof collapsed === "boolean")
            useViewerStore.getState().setNavCollapsed(teamKey, collapsed);
        }
        delete filtered.navCollapsed;
      }
      // SW-RP-1: rightPaneLayout is the one structured key an agent can write —
      // coerce it (the renderer assumes a well-formed shape). A malformed layout
      // is dropped rather than crashing RightPaneStack on the next render.
      // Reconcile to the built-ins like the load path, so an MCP write can't
      // restore a retired or unregistered panel's section.
      if ("rightPaneLayout" in filtered) {
        const coerced = coerceRightPaneLayout(filtered.rightPaneLayout);
        if (coerced) filtered.rightPaneLayout = reconcileLayoutToBuiltins(coerced);
        else delete filtered.rightPaneLayout;
      }
      // The agent decides the CLI's layout and visibility: its write is honoured as asked,
      // keeping the pairing `setLayoutMode` keeps. It may leave full screen or fold the CLI.
      if ("cliChrome" in filtered) {
        const invalid = invalidCliChromePatchFields(filtered.cliChrome);
        if (invalid.length) {
          logger.warn("mcp", `Dropped invalid cliChrome fields: ${invalid.join(", ")}`);
        }
        const current = cliChromeOf(useViewerStore.getState());
        const chrome = applyCliChromePatch(current, filtered.cliChrome);
        if (JSON.stringify(chrome) === JSON.stringify(current)) {
          logger.warn("mcp", "cliChrome patch changed no fields (invalid or already applied)");
        }
        delete filtered.cliChrome;
        Object.assign(filtered, cliChromeState(chrome));
      }
      if (filtered.settings && typeof filtered.settings === "object") {
        filtered.settings = { ...useViewerStore.getState().settings, ...filtered.settings };
      }
      if (Object.keys(filtered).length > 0) useViewerStore.setState(filtered);
      // The backend mirror merged the whole patch, so a save also pushes the real state back
      // over a value dropped here.
      useViewerStore.getState().persistState();
    });

    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
  useEffect(() => {
    const unlisten = listen<import("@/features/viewer/lib/workspace-groups").GroupSnapshot>(
      "mcp:workspace-groups-changed",
      (event) => {
        useViewerStore.getState().applyWorkspaceGroups(event.payload);
      },
    );
    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
  useEffect(() => {
    // close_workspace dropped the row from the saved state; drop this copy too, or
    // the next save writes it back and the pill returns.
    const unlisten = listen<RetiredWorkspace>("mcp:workspace-retired", (event) => {
      const store = useViewerStore.getState();
      const { open, minimized } = retiredWorkspaceIds(store, event.payload);
      if (open.length === 0 && minimized.length === 0) return;
      const { teamKey } = event.payload;
      // Its sessions already ended; these remove the rows and anything still attached.
      // The backend retired the row whatever its kind, so the renderer's
      // user-closable guard does not apply (a root is minted again as usual).
      for (const id of open) {
        store.closeWorkspace(teamKey, id, { killTerminals: true, bypassRootGuard: true });
      }
      for (const id of minimized) store.killTerminalsForSoftClosedWorkspace(teamKey, id);
      useViewerStore.getState().persistState();
    });

    return () => {
      unlisten.then(safeUnlisten).catch(() => {});
    };
  }, []);
}
