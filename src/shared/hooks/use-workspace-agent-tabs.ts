// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useState } from "react";

import { sessionHasLiveAgent } from "@/tauri/commands";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import {
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  useViewerStore,
  type TerminalTab,
  type TerminalTabIndex,
} from "@/shared/stores/viewer-store";

/**
 * SW-GS-2d (AC-3c/4/4a/5) — the generic in-surface session picker data layer.
 * Lists the agent-bearing tabs of ONE workspace instance and annotates each with
 * live-agent liveness, so the chat surface can offer "Connect to an open tab"
 * WITHOUT a tray trip. Parameterized by `(targetWorkspace, agentBasename)` — NO
 * git-sync/Claude literal — so any workspace agent surface reuses it (the picker
 * is scoped to the instance the surface resolved, e.g. `story:<path>`).
 *
 * Non-blocking (G3): the tab list is derived synchronously; liveness is probed
 * per-tab in the background and streamed in (`● live` / `◐ not-responding` /
 * probing), never awaited before the list paints.
 */

/** A tab in the resolved workspace bucket (shape the picker needs). */
export interface WorkspaceTab {
  id: string;
  name: string;
  sessionId: string;
  role?: string;
}

/** A pickable option: a workspace tab plus its liveness (`true` ●, `false` ◐,
 *  `null` still probing). */
export interface AgentTabOption extends WorkspaceTab {
  live: boolean | null;
}

/** The single tab → `WorkspaceTab` projection (drops `aiStatus` etc.). */
function toWorkspaceTab(t: TerminalTab): WorkspaceTab {
  return { id: t.id, name: t.name, sessionId: t.sessionId, role: t.role };
}

/** Pure: the tabs of `targetWorkspace` in `index`. Scoped to the instance — a
 *  different workspace id is a different bucket. */
export function listWorkspaceAgentTabs(
  index: TerminalTabIndex,
  teamKey: string,
  targetWorkspace: string,
): WorkspaceTab[] {
  return (index[teamKey]?.[targetWorkspace] ?? []).map(toWorkspaceTab);
}

/** Pure: shape tabs into picker options, applying the AC-4 connect-list filter.
 *  A tab is a candidate iff it carries a role tag (a spawned agent tab — shown
 *  ◐ while not-responding) OR it is PROVEN live (a user-started agent in an
 *  otherwise untagged tab). A roleless bare shell with no live agent is excluded
 *  — starting an agent in one is the distinct explicit action (AC-6). */
export function buildAgentTabOptions(
  tabs: WorkspaceTab[],
  liveness: Record<string, boolean | null>,
): AgentTabOption[] {
  return tabs
    .map((t) => ({ ...t, live: liveness[t.sessionId] ?? null }))
    .filter((o) => o.role !== undefined || o.live === true);
}

/**
 * The React hook: reactive workspace tabs + streamed liveness for the active
 * team's `targetWorkspace`. Returns the filtered picker options and a `refresh`
 * to re-probe (e.g. after Start-new). Team is resolved REACTIVELY
 * (`useActiveTeamKey`, SW-AW-6 AC-2): the app-level agent mounts persist across
 * team switches (SW-AW-5), so the bucket must re-derive on a switch without a
 * remount.
 */
export function useWorkspaceAgentTabs(
  targetWorkspace: string,
  agentBasename: string,
): { options: AgentTabOption[]; refresh: () => void } {
  const teamKey = useActiveTeamKey();
  // SW-GS-2d: the agent workspace runs MINIMIZED — its tabs are read the same way,
  // so the picker still lists them. Tabs Hidden from the tile are still running
  // agents — the picker lists them too.
  const shownBucket = useViewerStore((s) => selectWorkspaceTabs(s, teamKey, targetWorkspace));
  const hiddenBucket = useViewerStore((s) => selectHiddenTerminalTabs(s, teamKey, targetWorkspace));
  const bucket = useMemo(
    () => (hiddenBucket.length ? [...shownBucket, ...hiddenBucket] : shownBucket),
    [shownBucket, hiddenBucket],
  );
  const [liveness, setLiveness] = useState<Record<string, boolean | null>>({});
  const [refreshCounter, setRefreshCounter] = useState(0);
  const refresh = useCallback(() => setRefreshCounter((n) => n + 1), []);

  const tabs = useMemo<WorkspaceTab[]>(() => (bucket ?? []).map(toWorkspaceTab), [bucket]);
  // A stable key over the tab session ids so the probe effect re-runs only when
  // the set of tabs actually changes (not on every unrelated store update).
  const sessionKey = tabs.map((t) => t.sessionId).join(",");

  useEffect(() => {
    let cancelled = false;
    const sessionIds = sessionKey ? sessionKey.split(",") : [];
    const current = new Set(sessionIds);
    // Prune liveness to the current tab set (bounded growth) — also covers the
    // "all tabs gone" case where the per-probe updater below never runs. Deferred
    // to a microtask so it isn't a synchronous setState in the effect body
    // (cascading-render lint); identity-stable when nothing changed (no render).
    queueMicrotask(() => {
      if (cancelled) return;
      setLiveness((prev) => {
        const kept = Object.keys(prev).filter((k) => current.has(k));
        if (kept.length === Object.keys(prev).length) return prev;
        return Object.fromEntries(kept.map((k) => [k, prev[k]]));
      });
    });
    // SW-AN-6: an empty needle means "the agent identity isn't known yet" (the
    // adapter-derived basename during the brief cold window before the agent list
    // resolves). Skip probing — an empty needle would false-positive against any
    // process argv with an empty path segment. Liveness stays as-is and self-heals
    // when the basename resolves (this effect re-runs; `agentBasename` is a dep).
    if (!agentBasename) return;
    // Stream each probe in as it resolves (G3: never block the list on all probes).
    sessionIds.forEach(async (sid) => {
      let live = false;
      try {
        live = await sessionHasLiveAgent(sid, agentBasename);
      } catch {
        live = false;
      }
      if (cancelled) return;
      setLiveness((prev) => {
        const next: Record<string, boolean | null> = {};
        for (const k of Object.keys(prev)) if (current.has(k)) next[k] = prev[k];
        next[sid] = live;
        return next;
      });
    });
    return () => {
      cancelled = true;
    };
  }, [sessionKey, agentBasename, refreshCounter]);

  const options = useMemo(() => buildAgentTabOptions(tabs, liveness), [tabs, liveness]);
  return { options, refresh };
}
