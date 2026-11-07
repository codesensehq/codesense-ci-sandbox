// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";
import { useMachinesStore } from "@/features/machines/stores/machines-store";

import { useOwnsScope } from "@/shared/lib/warm-scopes";
import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { selectWorkspaceTabs, useViewerStore } from "@/shared/stores/viewer-store";
import { useContextOperationsStore } from "@/features/shell/stores/context-operations-store";

import { useTeamEditsStore } from "./team-edits-store";

/** Reads on window focus are at least this far apart. */
export const FOCUS_MIN_MS = 15_000;
/** A machine workspace is counted on focus at most this often. */
export const REMOTE_FOCUS_MIN_MS = 60_000;

/**
 * SW-XS-27: keep the team's unsaved team edits current, with no timer. It reads:
 * - when the team is opened;
 * - when the window gets focus (at most every 15 s; a connected machine's workspace
 *   is counted again when its last count is a minute old or more);
 * - when an explicit operation (a publish, a discard, a copy) settles;
 * - when the list is opened (the button reads on open);
 * - when an agent ends its turn (This Mac: a read; a machine workspace: counted).
 * A disconnected machine is never asked (the backend refuses to dial it).
 */
export function useTeamEditsRefresh(teamKey: string): void {
  const lastFocus = useRef(0);
  // A hidden team reads nothing and listens to nothing; it reads once when it is shown again.
  const active = useOwnsScope();

  useEffect(() => {
    if (teamKey === NO_TEAM_SENTINEL || !active) return;
    const store = useTeamEditsStore.getState();
    void store.refresh(teamKey);

    const onFocus = () => {
      const now = Date.now();
      if (now - lastFocus.current < FOCUS_MIN_MS) return;
      lastFocus.current = now;
      // Every machine workspace, clean ones too: a clean one may have changed.
      void useTeamEditsStore.getState().refreshAll(teamKey, REMOTE_FOCUS_MIN_MS);
    };
    window.addEventListener("focus", onFocus);

    // An operation settled: what it published or discarded changes the count.
    let settled = new Set(
      Object.values(useContextOperationsStore.getState().byId)
        .filter((op) => op.settledAt !== undefined)
        .map((op) => op.id),
    );
    let pending: ReturnType<typeof setTimeout> | null = null;
    /** One read shortly after a burst of changes. */
    const schedule = () => {
      if (pending) clearTimeout(pending);
      pending = setTimeout(() => {
        pending = null;
        void useTeamEditsStore.getState().refresh(teamKey);
      }, 400);
    };
    const unsubMachines = useMachinesStore.subscribe((state, previous) => {
      if (state.byTeam[teamKey] !== previous.byTeam[teamKey]) schedule();
    });
    const unsubOps = useContextOperationsStore.subscribe((s) => {
      const now = new Set(
        Object.values(s.byId)
          .filter((op) => op.settledAt !== undefined)
          .map((op) => op.id),
      );
      const fresh = [...now].some((id) => !settled.has(id));
      settled = now;
      if (!fresh) return;
      schedule();
    });

    // An agent ended its turn: what it wrote changes the count. On This Mac a
    // read counts the local workspaces; a machine workspace is counted itself.
    let running = new Set(
      Object.entries(useViewerStore.getState().aiStatusBySession)
        .filter(([, st]) => st === "running")
        .map(([sid]) => sid),
    );
    const unsubTurns = useViewerStore.subscribe((s) => {
      const nowRunning = new Set(
        Object.entries(s.aiStatusBySession)
          .filter(([, st]) => st === "running")
          .map(([sid]) => sid),
      );
      const ended = [...running].filter((sid) => !nowRunning.has(sid));
      running = nowRunning;
      if (ended.length === 0) return;
      let local = false;
      for (const ws of s.workspaces[teamKey] ?? []) {
        const tabs = selectWorkspaceTabs(s, teamKey, ws.id);
        if (!tabs.some((t) => t.sessionId && ended.includes(t.sessionId))) continue;
        if (isRemoteWorkspace(ws)) {
          if (!ws.location.handle) continue;
          void useTeamEditsStore.getState().refresh(teamKey, {
            kind: "remote",
            machineId: ws.location.machineId,
            workspaceHandle: ws.location.handle,
          });
        } else {
          local = true;
        }
      }
      if (local) schedule();
    });

    return () => {
      window.removeEventListener("focus", onFocus);
      unsubOps();
      unsubMachines();
      unsubTurns();
      if (pending) clearTimeout(pending);
    };
  }, [teamKey, active]);
}
