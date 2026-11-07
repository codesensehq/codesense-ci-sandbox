// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";

import { logger } from "@/shared/lib/logger";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useSprintRegistryRefresh } from "@/shared/stores/sprint-registry-refresh-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { loadSprintRegistry, type SprintRegistry } from "@/tauri/commands";

export interface UseSprintRegistry {
  /** The loaded registry for the CURRENT team, or `null` until its load resolves
   *  (or when the team has no resolvable org / no team name — a safe empty
   *  degradation). Never the previous team's registry (see `loading`). */
  registry: SprintRegistry | null;
  /** True while the settled result isn't for the current team yet — initial
   *  mount, or an in-flight team/org switch. Consumers gate the AC-6b "no active
   *  sprint" state on `!loading` so a still-loading registry never flashes the CTA. */
  loading: boolean;
}

/**
 * SW-SB-3 · Load a team's sprint registry (SW-SB-2 `loadSprintRegistry`) for the
 * board sprint selector. The Rust side reads the `sprints/` dir on the blocking
 * pool (`await_blocking`), so this is a fire-after-paint `invoke` that never
 * blocks the UI thread.
 *
 * The org root is resolved REACTIVELY off the org store (not a one-shot
 * `getState()` read): on cold boot the org store often hydrates AFTER the board
 * first mounts, so a non-reactive read would resolve `""`, settle empty, and —
 * since `teamKey` never changes — never retry, stranding the board on the
 * no-active-sprint path. Subscribing means the load re-fires the moment the org
 * appears. A stale in-flight load is cancel-guarded so a fast team switch can't
 * clobber the newer team's data.
 *
 * The load is tolerant by construction (a missing dir / malformed file yields an
 * empty registry + warnings, never a rejection), so there is no error state —
 * the empty registry drives the board's no-active-sprint path (AC-6b).
 *
 * Shared seam: SW-SB-19's left-nav sprint filter consumes this same hook.
 */
export function useSprintRegistry(teamKey: string): UseSprintRegistry {
  // Reactive org-root resolution — mirrors `resolveOrgRootForTeam` but as a
  // store subscription so a late org hydration re-runs the effect below.
  const orgRoot = useOrgStore((s) => {
    const { orgSlug } = parseTeamKey(teamKey);
    return s.orgs.find((o) => o.slug === orgSlug)?.org_root ?? "";
  });

  // SW-SB-11: a lifecycle write (create/close/promote/delete) for THIS team bumps
  // its nonce to force a re-load in place — teamKey/orgRoot are unchanged, so it's
  // the only signal that the just-written sprint file should re-appear. Keyed by
  // teamKey so a write in another team doesn't re-fetch this team's registry.
  const refreshNonce = useSprintRegistryRefresh((s) => s.nonces[teamKey] ?? 0);

  // SW-LN-B33: `config/sprints.yaml` also changes outside the app (a teammate's
  // pull, an agent), and nothing watches that file. Re-read it when this team's
  // content rescans (a pull lands as a content wave, which bumps the team's repo
  // keys) and whenever the window regains focus, so sprint chips, the Active
  // filter and the Plan defaults catch up without a restart. The read is cheap,
  // off the UI thread and tolerant.
  const teamScanTick = useFileRefreshStore((s) => {
    const { orgSlug, teamName } = parseTeamKey(teamKey);
    if (teamName === null) return 0;
    const prefix = `workspace:${orgSlug}:${teamName}:`;
    let sum = 0;
    for (const [key, n] of Object.entries(s.fileRefreshKeys)) {
      if (key.startsWith(prefix)) sum += n;
    }
    return sum;
  });
  const [focusNonce, setFocusNonce] = useState(0);
  useEffect(() => {
    const onFocus = () => setFocusNonce((n) => n + 1);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  // The settled result is tagged with the (teamKey, orgRoot) it belongs to, so
  // `loading` and `registry` can be DERIVED — no separate flag to fall out of
  // sync, no window where the previous team's registry shows as the new team's,
  // and (tagging orgRoot too) no window where a cold-boot org hydration re-fires
  // the load yet `loading` reads false because the same team already settled empty.
  const [loaded, setLoaded] = useState<{
    key: string;
    orgRoot: string;
    registry: SprintRegistry | null;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    const { teamName } = parseTeamKey(teamKey);

    async function loadForTeam() {
      // Genuinely no team → settle empty (not loading); nothing will ever load.
      if (teamName === null) {
        if (!cancelled) setLoaded({ key: teamKey, orgRoot, registry: null });
        return;
      }
      // A real team whose org root hasn't resolved yet (cold boot, still
      // hydrating): do NOT settle — stay in the loading state. The effect
      // re-fires the moment `orgRoot` appears (it's in the dep array), and a
      // real teamKey always implies an org will arrive.
      if (!orgRoot) return;
      try {
        const registry = await loadSprintRegistry(orgRoot, teamName);
        if (!cancelled) setLoaded({ key: teamKey, orgRoot, registry });
      } catch (e: unknown) {
        // The command never rejects on content problems; a reject here is an IPC
        // fault. Degrade to an empty registry so the board still renders.
        if (!cancelled) {
          logger.warn("viewer", `load sprint registry failed (teamKey=${teamKey}): ${String(e)}`);
          setLoaded({
            key: teamKey,
            orgRoot,
            registry: { activeSprint: null, sprints: [], warnings: [] },
          });
        }
      }
    }

    void loadForTeam();
    return () => {
      cancelled = true;
    };
  }, [teamKey, orgRoot, refreshNonce, focusNonce, teamScanTick]);

  const current = loaded && loaded.key === teamKey && loaded.orgRoot === orgRoot ? loaded : null;
  return { registry: current?.registry ?? null, loading: current === null };
}
