// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";

import { logger } from "@/shared/lib/logger";
import { parseTeamKey } from "@/shared/lib/parse-team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useStatusTaxonomyRefresh } from "@/shared/stores/status-taxonomy-refresh-store";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import type { StatusState } from "@/tauri/commands";
import { DEFAULT_STATUS_STATES } from "@/shared/lib/pipeline-graph/status-config";
import { loadStatusTaxonomy } from "@/tauri/commands";

/**
 * SW-GT-3 · Hydrate the status-taxonomy snapshot store for the active team.
 *
 * Mounted once high in the tree (App) so the snapshot is populated for EVERY
 * switcher/board surface, not just the board. The store is already seeded with
 * `DEFAULT_STATUS_STATES` at import, so this hook never gates first paint — it
 * only swaps in a team's CUSTOM taxonomy once the async load settles.
 *
 * Mirrors `use-sprint-registry`:
 *   - org root is resolved REACTIVELY off `useOrgStore` (a one-shot `getState()`
 *     read would settle `""` on cold boot — org hydrates AFTER first mount — and,
 *     since `teamKey` never changes, never retry). Subscribing re-fires the load
 *     the moment the org appears.
 *   - the load is cancel-guarded per (team, orgRoot) so a fast team switch can't
 *     clobber the newer team's snapshot (AC-3 cancel-guarded per-team load).
 *   - `loadStatusTaxonomy` runs on the Rust blocking pool (off the webview thread)
 *     and never rejects on content problems — a broken file falls back to the
 *     shipped defaults + warnings. A reject here is an IPC fault; we degrade to the
 *     default seed so the switchers/board keep working.
 *   - a `status-taxonomy-refresh` nonce (bumped by a future in-session editor)
 *     forces a re-load in place without an app restart.
 */
export function useStatusTaxonomy(teamKey: string): void {
  const setStates = useStatusTaxonomyStore((s) => s.setStates);
  useLoadedTeamTaxonomy(teamKey, setStates);
}

/**
 * A team's own loaded taxonomy, through the same load path as `useStatusTaxonomy`
 * but held per caller instead of in the active-team snapshot — so a retained team
 * never reads another team's states. `null` until the first load settles.
 */
export function useTeamStatusStates(teamKey: string): Map<string, StatusState> | null {
  const [states, setStates] = useState<StatusState[] | null>(null);
  useLoadedTeamTaxonomy(teamKey, setStates);
  return useMemo(() => (states ? new Map(states.map((s) => [s.id, s])) : null), [states]);
}

function useLoadedTeamTaxonomy(teamKey: string, setStates: (states: StatusState[]) => void): void {
  const orgRoot = useOrgStore((s) => {
    const { orgSlug } = parseTeamKey(teamKey);
    return s.orgs.find((o) => o.slug === orgSlug)?.org_root ?? "";
  });
  const refreshNonce = useStatusTaxonomyRefresh((s) => s.nonces[teamKey] ?? 0);

  useEffect(() => {
    let cancelled = false;
    const { teamName } = parseTeamKey(teamKey);

    async function loadForTeam(): Promise<void> {
      // No resolvable team → keep the default seed (nothing to load).
      if (teamName === null) {
        if (!cancelled) setStates(DEFAULT_STATUS_STATES);
        return;
      }
      // Real team whose org root hasn't resolved yet (cold boot): don't settle —
      // the effect re-fires the moment `orgRoot` appears (it's in the dep array).
      if (!orgRoot) return;
      try {
        const taxonomy = await loadStatusTaxonomy(orgRoot, teamName);
        if (cancelled) return;
        // A tolerant load can (in principle) return no states; never blank the
        // switchers/board — fall back to the shipped seed.
        setStates(taxonomy.states.length > 0 ? taxonomy.states : DEFAULT_STATUS_STATES);
      } catch (e: unknown) {
        if (cancelled) return;
        logger.warn("viewer", `load status taxonomy failed (teamKey=${teamKey}): ${String(e)}`);
        setStates(DEFAULT_STATUS_STATES);
      }
    }

    void loadForTeam();
    return () => {
      cancelled = true;
    };
  }, [teamKey, orgRoot, refreshNonce, setStates]);
}
